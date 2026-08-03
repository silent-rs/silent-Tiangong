//! Index sidecar 业务服务：承载 tantivy 索引、后台扫描与 rg/grep 检索，按操作名分发请求。
//!
//! 整合原 plugin.rs 的生命周期钩子（set_workspace / index_turn_batch / finalize）与
//! handler.rs 的工具执行（index_search / search_code）+ 管理 API，全部经 IPC 操作
//! 暴露给运行时（host 侧 invoke_sidecar）与 WASM 桥接。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow};

use tiangong_plugin_index_protocol::lifecycle::{
    FINALIZE_SESSION_OPERATION, FinalizeSessionRequest, INDEX_TURN_BATCH_OPERATION,
    IndexTurnBatchRequest, SET_WORKSPACE_OPERATION, SetWorkspaceRequest,
};
use tiangong_plugin_index_protocol::management::{
    DELETE_WORKSPACE_INDEX_OPERATION, DeleteWorkspaceIndexRequest,
    LIST_WORKSPACE_INDEXES_OPERATION, PREWARM_WORKSPACE_INDEX_OPERATION,
    PrewarmWorkspaceIndexRequest, REBUILD_WORKSPACE_INDEX_OPERATION, RebuildWorkspaceIndexRequest,
    RebuildWorkspaceIndexResponse,
};
use tiangong_plugin_index_protocol::search::{
    INDEX_SEARCH_OPERATION, IndexSearchRequest, IndexSearchResponse, SEARCH_CODE_OPERATION,
    SearchCodeRequest, SearchCodeResponse,
};
use tiangong_plugin_index_protocol::{
    INDEX_PROTOCOL_VERSION, IndexScope, PLUGIN_ID, PLUGIN_VERSION, WorkspaceIndexInfo,
};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};
use tiangong_toolkit as shared;

use crate::index::{IndexManager, IndexQuery};

/// Index sidecar 业务服务。
pub struct IndexService {
    manager: Arc<IndexManager>,
    /// 当前会话工作目录（由 set_workspace 注入）。
    workspace: RwLock<Option<PathBuf>>,
}

impl IndexService {
    /// 用默认存储路径构造。
    pub fn new() -> Result<Self> {
        let manager = Arc::new(IndexManager::new()?);
        Ok(Self {
            manager,
            workspace: RwLock::new(None),
        })
    }

    /// 按 sidecar 协议分发请求。
    ///
    /// `async`：慢操作（full_scan / search_code 子进程）经 `spawn_blocking` 在独立
    /// 线程执行，避免在单线程 runtime 上阻塞其他连接的请求与健康检查。
    pub async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "Index 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                    request.protocol_version
                ),
                false,
            );
        }

        let payload = match self
            .dispatch_operation(&request.operation, request.payload)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                return Response::error(
                    &request_id,
                    ErrorCode::ServiceError,
                    error.to_string(),
                    false,
                );
            }
        };
        Response::success(&request_id, payload)
    }

    async fn dispatch_operation(
        &self,
        operation: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        match operation {
            HANDSHAKE_OPERATION => serde_json::to_value(HandshakeResponse {
                plugin_id: PLUGIN_ID.to_string(),
                plugin_version: PLUGIN_VERSION.to_string(),
                sidecar_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol_version: PROTOCOL_VERSION.to_string(),
                business_protocol: INDEX_PROTOCOL_VERSION,
                capabilities: vec!["index".to_string()],
                instance_id: format!("index-sidecar-{}", std::process::id()),
                status: ServiceStatus::Ready,
            })
            .with_context(|| "序列化 Index 握手响应失败"),
            INDEX_SEARCH_OPERATION => {
                let req: IndexSearchRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 index_search 请求失败")?;
                let manager = self.manager.clone();
                let cwd = self.resolve_workspace(req.workspace.clone());
                let resp = tokio::task::spawn_blocking(move || {
                    handle_index_search_blocking(&manager, &cwd, req)
                })
                .await
                .with_context(|| "index_search 后台任务失败")??;
                serde_json::to_value(resp).with_context(|| "序列化 index_search 响应失败")
            }
            SEARCH_CODE_OPERATION => {
                let req: SearchCodeRequest =
                    serde_json::from_value(payload).with_context(|| "解析 search_code 请求失败")?;
                let base = self.resolve_workspace(req.workspace.clone());
                let resp =
                    tokio::task::spawn_blocking(move || handle_search_code_blocking(&base, req))
                        .await
                        .with_context(|| "search_code 后台任务失败")?;
                serde_json::to_value(resp).with_context(|| "序列化 search_code 响应失败")
            }
            SET_WORKSPACE_OPERATION => {
                let req: SetWorkspaceRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 set_workspace 请求失败")?;
                self.handle_set_workspace(req.workspace)?;
                serde_json::to_value(tiangong_plugin_index_protocol::Ack {})
                    .with_context(|| "序列化 set_workspace 响应失败")
            }
            INDEX_TURN_BATCH_OPERATION => {
                let req: IndexTurnBatchRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 index_turn_batch 请求失败")?;
                let manager = self.manager.clone();
                tokio::task::spawn_blocking(move || {
                    manager.index_turn_batch(&req.session_id, &req.turns)
                })
                .await
                .with_context(|| "index_turn_batch 后台任务失败")??;
                serde_json::to_value(tiangong_plugin_index_protocol::Ack {})
                    .with_context(|| "序列化 index_turn_batch 响应失败")
            }
            FINALIZE_SESSION_OPERATION => {
                let req: FinalizeSessionRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 finalize_session 请求失败")?;
                let manager = self.manager.clone();
                tokio::task::spawn_blocking(move || {
                    manager.finalize_session_index(&req.session_id)
                })
                .await
                .with_context(|| "finalize_session 后台任务失败")??;
                serde_json::to_value(tiangong_plugin_index_protocol::Empty {})
                    .with_context(|| "序列化 finalize_session 响应失败")
            }
            LIST_WORKSPACE_INDEXES_OPERATION => {
                let manager = self.manager.clone();
                let infos = tokio::task::spawn_blocking(move || manager.list_workspace_indexes())
                    .await
                    .with_context(|| "list_workspace_indexes 后台任务失败")??;
                let resp: Vec<WorkspaceIndexInfo> = infos
                    .into_iter()
                    .map(|info| WorkspaceIndexInfo {
                        id: info.id,
                        root: info.root,
                        entry_count: info.entry_count,
                        updated_at: info.updated_at,
                    })
                    .collect();
                serde_json::to_value(resp).with_context(|| "序列化 list_workspace_indexes 响应失败")
            }
            DELETE_WORKSPACE_INDEX_OPERATION => {
                let req: DeleteWorkspaceIndexRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 delete_workspace_index 请求失败")?;
                let manager = self.manager.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let root = Path::new(&req.root);
                    // 取得扫描 permit 与后台扫描/重建互斥，避免删除时后台线程继续写已移除目录。
                    let _permit = wait_for_scan_permit(&manager, root)?;
                    manager.delete_workspace_index(root, &req.workspace_id)
                })
                .await
                .with_context(|| "delete_workspace_index 后台任务失败")??;
                serde_json::to_value(tiangong_plugin_index_protocol::Empty {})
                    .with_context(|| "序列化 delete_workspace_index 响应失败")
            }
            REBUILD_WORKSPACE_INDEX_OPERATION => {
                let req: RebuildWorkspaceIndexRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 rebuild_workspace_index 请求失败")?;
                let manager = self.manager.clone();
                // 异步重建：立即返回「已排队」，后台用双索引切换（零阻塞搜索）执行。
                // rebuild 构建到临时目录，完成后原子替换，搜索在重建期间继续命中旧索引。
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let root = Path::new(&req.root);
                    let _permit = wait_for_scan_permit(&manager, root)?;
                    match manager.rebuild(root) {
                        Ok(count) => {
                            tracing::info!(count, "工作区索引重建完成（双索引切换）");
                        }
                        Err(error) => {
                            tracing::error!(%error, "工作区索引重建失败");
                        }
                    }
                    Ok(())
                })
                .await
                .ok();
                serde_json::to_value(RebuildWorkspaceIndexResponse {
                    queued: true,
                    count: 0,
                })
                .with_context(|| "序列化 rebuild_workspace_index 响应失败")
            }
            PREWARM_WORKSPACE_INDEX_OPERATION => {
                let req: PrewarmWorkspaceIndexRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 prewarm_workspace_index 请求失败")?;
                let manager = self.manager.clone();
                let root = PathBuf::from(&req.root);
                tokio::task::spawn_blocking(move || handle_prewarm_blocking(manager, &root))
                    .await
                    .with_context(|| "prewarm_workspace_index 后台任务失败")??;
                serde_json::to_value(tiangong_plugin_index_protocol::Empty {})
                    .with_context(|| "序列化 prewarm_workspace_index 响应失败")
            }
            operation => Err(anyhow!("不支持的 Index 操作: {operation}")),
        }
    }

    /// 解析本次请求实际使用的工作目录：优先请求携带的，回退全局缓存。
    ///
    /// 全局 workspace 仅由 `set_workspace` 钩子写入，用于触发后台扫描；查询时必须
    /// 按请求路由，避免同一 sidecar 服务多个不同工作区的会话时互相串用。
    fn resolve_workspace(&self, request_workspace: Option<String>) -> Option<PathBuf> {
        request_workspace
            .map(PathBuf::from)
            .or_else(|| self.workspace())
    }

    // ── 生命周期 ─────────────────────────────────────────────

    fn handle_set_workspace(&self, workspace: Option<String>) -> Result<()> {
        let new_path = workspace.map(PathBuf::from);
        if let Ok(mut guard) = self.workspace.write() {
            *guard = new_path.clone();
        }
        if let Some(ref root) = new_path
            && root.is_dir()
            && !self.manager.is_workspace_scanning(root)
        {
            self.refresh_workspace(root);
        }
        Ok(())
    }

    /// 首次打开或成功扫描超过一小时后触发增量刷新。
    fn refresh_workspace(&self, root: &Path) {
        const STALE_THRESHOLD_SECS: u64 = 3600;
        let needs_refresh = !self.manager.workspace_index_exists(root)
            || self
                .manager
                .workspace_index_age_secs(root)
                .is_none_or(|age| age > STALE_THRESHOLD_SECS);
        if needs_refresh {
            self.spawn_background_scan(root);
        }
    }

    fn spawn_background_scan(&self, root: &Path) {
        let Some(permit) = self.manager.try_begin_workspace_scan(root) else {
            tracing::debug!(
                workspace = %root.display(),
                "Workspace 索引已有后台扫描在进行，跳过本次"
            );
            return;
        };
        let manager = self.manager.clone();
        let root = root.to_path_buf();
        tracing::info!(workspace = %root.display(), "Workspace 索引后台扫描启动");
        std::thread::spawn(move || {
            // permit 持有期间状态保持占用；drop（含 panic 展开）时自动复位。
            let _permit = permit;
            match manager.incremental_scan(&root) {
                Ok(count) => tracing::info!(count, "Workspace 索引后台增量刷新完成"),
                Err(e) => tracing::warn!("Workspace 索引后台增量刷新失败: {e}"),
            }
        });
    }

    // ── 辅助 ─────────────────────────────────────────────────────

    fn workspace(&self) -> Option<PathBuf> {
        self.workspace.read().ok()?.clone()
    }
}

/// 读路径解析（search_code 用），与原 fs 插件一致的信任模式语义。
fn resolve_read_path(raw: &str, base: &Path, full_trust: bool) -> Result<PathBuf> {
    if full_trust {
        shared::resolve_workspace_path_trusted_with(raw, base)
    } else {
        shared::resolve_workspace_path_with(raw, base)
    }
}

/// index_search 阻塞实现（在 spawn_blocking 线程内执行）。
fn handle_index_search_blocking(
    manager: &IndexManager,
    cwd: &Option<PathBuf>,
    req: IndexSearchRequest,
) -> Result<IndexSearchResponse> {
    let limit = if req.limit == 0 {
        10
    } else {
        req.limit.clamp(1, 20)
    };
    let scope = req.scope;

    let mut workspace_hits = Vec::new();
    let scanning = false;

    // Workspace 索引查询
    //
    // 不再因后台扫描/rebuild 进行中而短路：后台 incremental 只在 commit 时短暂持锁，
    // rebuild 用双索引切换（不持有旧索引锁），搜索总能命中当前可用索引。
    if matches!(scope, IndexScope::Workspace | IndexScope::All)
        && let Some(cwd) = cwd
        && cwd.is_dir()
    {
        let index_query = IndexQuery::new(&req.query)
            .with_scope(IndexScope::Workspace)
            .with_limit(limit);
        match manager.search(cwd, &index_query) {
            Ok(hits) => workspace_hits = hits,
            Err(e) => {
                tracing::warn!(%e, "工作区搜索失败（索引可能正在初始化）");
            }
        }
    }

    // Session 索引查询
    let mut session_hits = Vec::new();
    if matches!(scope, IndexScope::Session | IndexScope::All)
        && let Some(session_id) = &req.session_id
    {
        match manager.search_session(session_id, &req.query, limit) {
            Ok(hits) => session_hits = hits,
            Err(e) => {
                tracing::warn!("对话搜索失败: {e}");
            }
        }
    }

    Ok(IndexSearchResponse {
        workspace_hits,
        session_hits,
        scanning,
    })
}

/// search_code 阻塞实现（在 spawn_blocking 线程内执行）。
fn handle_search_code_blocking(
    base: &Option<PathBuf>,
    req: SearchCodeRequest,
) -> SearchCodeResponse {
    let Some(base) = base else {
        return SearchCodeResponse {
            ok: false,
            summary: "会话工作目录未注入，无法执行检索".to_string(),
            stderr: "workspace not available".to_string(),
            ..Default::default()
        };
    };
    let pattern = req.pattern.trim();
    if pattern.is_empty() {
        return SearchCodeResponse {
            ok: false,
            summary: "search_code pattern 不能为空".to_string(),
            stderr: "empty pattern".to_string(),
            ..Default::default()
        };
    }
    let target = req.path.as_deref().unwrap_or(".");
    let full_path = match resolve_read_path(target, base, req.full_trust) {
        Ok(p) => p,
        Err(e) => {
            return SearchCodeResponse {
                ok: false,
                summary: format!("search_code 失败：{e}"),
                stderr: e.to_string(),
                ..Default::default()
            };
        }
    };

    let timeout_ms = shared::command_timeout_ms();
    let target_text = full_path.display().to_string();
    let rg_result = shared::execute_command_with_timeout(
        Command::new("rg")
            .arg("--line-number")
            .arg("--no-heading")
            .arg("--color")
            .arg("never")
            .arg(pattern)
            .arg(&target_text)
            .current_dir(base)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        timeout_ms,
    );

    let (output, timed_out) = match rg_result {
        Ok(payload) => payload,
        Err(_) => match shared::execute_command_with_timeout(
            Command::new("grep")
                .arg("-R")
                .arg("-n")
                .arg("-I")
                .arg("--exclude-dir=.git")
                .arg("--exclude-dir=target")
                .arg("--exclude-dir=node_modules")
                .arg("--exclude-dir=dist")
                .arg("--exclude-dir=build")
                .arg(pattern)
                .arg(&target_text)
                .current_dir(base)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        )
        .with_context(|| format!("执行代码检索失败：pattern={pattern}"))
        {
            Ok(p) => p,
            Err(e) => {
                return SearchCodeResponse {
                    ok: false,
                    summary: format!("代码检索失败：{e}"),
                    stderr: e.to_string(),
                    ..Default::default()
                };
            }
        },
    };

    let exit_code = if timed_out {
        -1
    } else {
        output.status.code().unwrap_or(-1)
    };
    let stdout = shared::truncate_output(&String::from_utf8_lossy(&output.stdout));
    let stderr = shared::truncate_output(&String::from_utf8_lossy(&output.stderr));
    let ok = !timed_out && (output.status.success() || exit_code == 1);
    let summary = if timed_out {
        format!("代码检索超时：pattern={pattern} (timeout_ms={timeout_ms})")
    } else if exit_code == 1 {
        format!("代码检索完成：未找到匹配（pattern={pattern}）")
    } else if ok {
        format!("代码检索成功：pattern={pattern}")
    } else {
        format!("代码检索失败：pattern={pattern} (exit_code={exit_code})")
    };

    SearchCodeResponse {
        ok,
        summary,
        stdout,
        stderr,
        exit_code: exit_code as i64,
    }
}

/// 等待取得工作区扫描 permit（与后台扫描/重建/删除互斥）。
///
/// 用于 delete/rebuild 操作：在已有后台扫描进行时短暂轮询等待（最多约 30s），
/// 拿到 permit 后返回；超时仍未取得则返回错误，避免无限阻塞累积后台任务。
fn wait_for_scan_permit(
    manager: &IndexManager,
    root: &Path,
) -> Result<crate::index::WorkspaceScanPermit> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(permit) = manager.try_begin_workspace_scan(root) {
            return Ok(permit);
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!("等待索引扫描许可超时，请稍后重试"));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// prewarm 阻塞实现（在 spawn_blocking 线程内执行）。
fn handle_prewarm_blocking(manager: Arc<IndexManager>, root: &Path) -> Result<()> {
    if !root.is_dir() || manager.workspace_index_exists(root) {
        return Ok(());
    }
    let Some(permit) = manager.try_begin_workspace_scan(root) else {
        return Ok(());
    };
    let manager = manager.clone();
    let root = root.to_path_buf();
    tracing::info!(workspace = %root.display(), "Workspace 索引预热启动");
    std::thread::spawn(move || {
        let _permit = permit;
        match manager.incremental_scan(&root) {
            Ok(count) => tracing::info!(count, "Workspace 索引预热完成"),
            Err(e) => tracing::warn!("Workspace 索引预热失败: {e}"),
        }
    });
    Ok(())
}

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for IndexService {
    async fn dispatch(
        &self,
        request: tiangong_plugin_runtime::protocol::Request,
    ) -> tiangong_plugin_runtime::protocol::Response {
        IndexService::dispatch(self, request).await
    }
}
