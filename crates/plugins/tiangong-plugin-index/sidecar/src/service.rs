//! Index sidecar 业务服务：承载 tantivy 索引、后台扫描与 rg/grep 检索，按操作名分发请求。
//!
//! 整合原 plugin.rs 的生命周期钩子（set_workspace / index_turn_batch / finalize）与
//! handler.rs 的工具执行（index_search / search_code）+ 管理 API，全部经 IPC 操作
//! 暴露给运行时（host 侧 invoke_sidecar）与 WASM 桥接。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::RwLock;

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
    manager: IndexManager,
    /// 当前会话工作目录（由 set_workspace 注入）。
    workspace: RwLock<Option<PathBuf>>,
    /// 上次已扫描的工作目录（避免同一目录重复扫描）。
    last_scanned: RwLock<Option<PathBuf>>,
}

impl IndexService {
    /// 用默认存储路径构造。
    pub fn new() -> Result<Self> {
        let manager = IndexManager::new()?;
        Ok(Self {
            manager,
            workspace: RwLock::new(None),
            last_scanned: RwLock::new(None),
        })
    }

    /// 按 sidecar 协议分发请求。
    pub fn dispatch(&self, request: Request) -> Response {
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

        let payload = match self.dispatch_operation(&request.operation, request.payload) {
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

    fn dispatch_operation(
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
                let resp = self.handle_index_search(req)?;
                serde_json::to_value(resp).with_context(|| "序列化 index_search 响应失败")
            }
            SEARCH_CODE_OPERATION => {
                let req: SearchCodeRequest =
                    serde_json::from_value(payload).with_context(|| "解析 search_code 请求失败")?;
                let resp = self.handle_search_code(req);
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
                self.handle_index_turn_batch(req)?;
                serde_json::to_value(tiangong_plugin_index_protocol::Ack {})
                    .with_context(|| "序列化 index_turn_batch 响应失败")
            }
            FINALIZE_SESSION_OPERATION => {
                let req: FinalizeSessionRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 finalize_session 请求失败")?;
                self.manager.finalize_session_index(&req.session_id)?;
                serde_json::to_value(tiangong_plugin_index_protocol::Empty {})
                    .with_context(|| "序列化 finalize_session 响应失败")
            }
            LIST_WORKSPACE_INDEXES_OPERATION => {
                let infos = self.manager.list_workspace_indexes()?;
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
                self.manager
                    .delete_workspace_index(Path::new(&req.root), &req.workspace_id)?;
                serde_json::to_value(tiangong_plugin_index_protocol::Empty {})
                    .with_context(|| "序列化 delete_workspace_index 响应失败")
            }
            REBUILD_WORKSPACE_INDEX_OPERATION => {
                let req: RebuildWorkspaceIndexRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 rebuild_workspace_index 请求失败")?;
                let count = self.manager.full_scan(Path::new(&req.root))?;
                serde_json::to_value(RebuildWorkspaceIndexResponse { count })
                    .with_context(|| "序列化 rebuild_workspace_index 响应失败")
            }
            PREWARM_WORKSPACE_INDEX_OPERATION => {
                let req: PrewarmWorkspaceIndexRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 prewarm_workspace_index 请求失败")?;
                self.handle_prewarm(Path::new(&req.root))?;
                serde_json::to_value(tiangong_plugin_index_protocol::Empty {})
                    .with_context(|| "序列化 prewarm_workspace_index 响应失败")
            }
            operation => Err(anyhow!("不支持的 Index 操作: {operation}")),
        }
    }

    // ── index_search ────────────────────────────────────────────

    fn handle_index_search(&self, req: IndexSearchRequest) -> Result<IndexSearchResponse> {
        let limit = if req.limit == 0 {
            10
        } else {
            req.limit.clamp(1, 20)
        };
        let scope = req.scope;

        let mut workspace_hits = Vec::new();
        let mut scanning = false;

        // Workspace 索引查询
        if matches!(scope, IndexScope::Workspace | IndexScope::All)
            && let Some(cwd) = self.workspace()
            && cwd.is_dir()
        {
            if self.manager.is_workspace_scanning(&cwd) {
                scanning = true;
            } else {
                let index_query = IndexQuery::new(&req.query)
                    .with_scope(IndexScope::Workspace)
                    .with_limit(limit);
                match self.manager.search(&cwd, &index_query) {
                    Ok(hits) => workspace_hits = hits,
                    Err(e) => {
                        tracing::warn!("工作区搜索失败: {e}");
                    }
                }
            }
        }

        // Session 索引查询
        let mut session_hits = Vec::new();
        if matches!(scope, IndexScope::Session | IndexScope::All)
            && let Some(session_id) = &req.session_id
        {
            match self.manager.search_session(session_id, &req.query, limit) {
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

    // ── search_code ─────────────────────────────────────────────

    fn handle_search_code(&self, req: SearchCodeRequest) -> SearchCodeResponse {
        let Some(base) = self.workspace() else {
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
        let full_path = match resolve_read_path(target, &base, req.full_trust) {
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
                .current_dir(&base)
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
                    .current_dir(&base)
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

    // ── 生命周期 ─────────────────────────────────────────────

    fn handle_set_workspace(&self, workspace: Option<String>) -> Result<()> {
        let new_path = workspace.map(PathBuf::from);
        if let Ok(mut guard) = self.workspace.write() {
            *guard = new_path.clone();
        }
        // 工作区变更后触发后台扫描（与原 plugin.rs set_workspace 逻辑一致）。
        if let Some(ref root) = new_path
            && root.is_dir()
        {
            let already_scanned = self
                .last_scanned
                .read()
                .map(|g| g.as_ref() == Some(root))
                .unwrap_or(false);
            let scanning = self.manager.is_workspace_scanning(root);
            if !already_scanned && !scanning {
                self.full_scan_workspace(root);
            }
        }
        Ok(())
    }

    /// 对工作区做全量扫描（索引不存在则后台扫描，已存在则复用+置位 last_scanned）。
    fn full_scan_workspace(&self, root: &Path) {
        if !root.is_dir() {
            return;
        }
        if !self.manager.workspace_index_exists(root) {
            self.spawn_background_scan(root);
            return;
        }
        if let Ok(mut guard) = self.last_scanned.write() {
            guard.clone_from(&Some(root.to_path_buf()));
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
        let manager = self.manager_clone();
        let root = root.to_path_buf();
        tracing::info!(workspace = %root.display(), "Workspace 索引后台扫描启动");
        std::thread::spawn(move || {
            // permit 持有期间状态保持占用；drop（含 panic 展开）时自动复位。
            let _permit = permit;
            match manager.full_scan(&root) {
                Ok(count) => tracing::info!(count, "Workspace 索引后台扫描完成"),
                Err(e) => tracing::warn!("Workspace 索引后台扫描失败: {e}"),
            }
        });
    }

    fn handle_index_turn_batch(&self, req: IndexTurnBatchRequest) -> Result<()> {
        if req.turns.is_empty() {
            return Ok(());
        }
        // protocol::TurnData 与 index::TurnData 已统一为同一类型（sidecar 内部复用 protocol），
        // 无需转换，直接传入 manager。
        self.manager.index_turn_batch(&req.session_id, &req.turns)
    }

    fn handle_prewarm(&self, root: &Path) -> Result<()> {
        if !root.is_dir() || self.manager.workspace_index_exists(root) {
            return Ok(());
        }
        let Some(permit) = self.manager.try_begin_workspace_scan(root) else {
            return Ok(());
        };
        let manager = self.manager_clone();
        let root = root.to_path_buf();
        tracing::info!(workspace = %root.display(), "Workspace 索引预热启动");
        std::thread::spawn(move || {
            let _permit = permit;
            match manager.full_scan(&root) {
                Ok(count) => tracing::info!(count, "Workspace 索引预热完成"),
                Err(e) => tracing::warn!("Workspace 索引预热失败: {e}"),
            }
        });
        Ok(())
    }

    // ── 辅助 ─────────────────────────────────────────────────────

    fn workspace(&self) -> Option<PathBuf> {
        self.workspace.read().ok()?.clone()
    }

    /// IndexManager 没有提供 clone（它内部是 Arc 缓存），但跨线程扫描需要重新
    /// 打开一个 manager 句柄。这里用一个独立 manager 指向同一 base_dir——后台
    /// 扫描的磁盘锁由 tantivy per-directory 守护，扫描去重由原 manager 的
    /// scanning_roots permit 守护（permit 在 spawn 线程持有）。
    fn manager_clone(&self) -> IndexManager {
        IndexManager::new_with_dir(self.manager.base_dir().to_path_buf())
            .expect("IndexManager 后台扫描实例初始化失败")
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
