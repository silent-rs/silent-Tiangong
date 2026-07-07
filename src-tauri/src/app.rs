use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tauri::Manager;

use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::CoreConfigProvider;
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;

/// 天工应用状态
///
/// state: 应用管理（会话列表、配置、持久化）— Arc<tokio Mutex> 以支持嵌入式 server 共享
/// cores: 活跃的对话核心（session_id → TiangongCore）
/// config: 共享配置提供者
/// embedded_server: 嵌入式 Server 句柄（Desktop 模式下 Server 运行在 app 进程内）
pub struct TiangongApp {
    pub state: std::sync::Arc<AsyncMutex<tiangong_app_state::app_state::TiangongState>>,
    pub cores: Mutex<HashMap<String, TiangongCore>>,
    /// 覆盖单个会话从附件准备到 Core 持久化确认的完整串行区间。
    /// 不同会话使用不同锁，可以并行发送；同一会话不会并发创建 Core 或抢占草稿。
    session_send_locks: Mutex<HashMap<String, std::sync::Arc<AsyncMutex<()>>>>,
    /// 草稿归档/持久化串行锁。与发送锁分离，使用户等待发送期间的新输入能立即写入
    /// 新 revision，而不会等旧发送结束后才落盘。
    draft_update_locks: Mutex<HashMap<String, std::sync::Arc<AsyncMutex<()>>>>,
    /// 临时草稿 ID 转正后的运行时重定向。迟到的旧 ID 写入会跟随到
    /// 真实会话，避免迁移后又复活一份孤立草稿。
    draft_redirects: Mutex<HashMap<String, String>>,
    /// 已成功投递的最新草稿 revision，用于防止双击或迟到请求重复发送。
    delivered_draft_revisions: Mutex<HashMap<String, u64>>,
    /// 前端冻结发送快照后、真正进入 send_message 前的附件租约。
    /// 该租约让草稿清理不会删掉已被本次发送冻结的归档路径。
    draft_send_claims: Mutex<HashMap<String, DraftSendClaim>>,
    /// 当前进程已明确丢弃/删除的草稿 ID，阻止早已发出的迟到写入复活。
    discarded_drafts: Mutex<HashSet<String>>,
    /// 活动会话变更代数，用于草稿转正后的条件激活。
    active_session_epoch: AtomicU64,
    pub config: CoreConfigProvider,
    scheduler_context: std::sync::Arc<crate::scheduler::DesktopSchedulerContext>,
    /// 插件集合变化（能力新增/删除）时标记的 session，下次 ensure_core 移除旧 core 重建。
    plugin_dirty_sessions: Mutex<HashSet<String>>,
    /// Skill 管理插件句柄（dual-ownership：core 拿 clone 做 LLM 工具，
    /// app 持有此句柄做 skill 管理：remove/set_enabled/refresh/gc/doctor）。
    pub skill_plugin: std::sync::Arc<tiangong_plugin_skill::SkillPlugin>,
    /// MCP 管理插件句柄（dual-ownership：core 拿 clone 做 LLM 工具（动态 MCP 工具
    /// spec + 执行分发），app 持有此句柄做 MCP 管理：register/update/remove/
    /// set_enabled/probe/health）。
    pub mcp_plugin: std::sync::Arc<tiangong_plugin_mcp::McpPlugin>,
    embedded_server: Mutex<Option<tiangong_server::EmbeddedServerHandle>>,
    /// Tauri 应用句柄（browser/terminal 插件构造需要）。
    ///
    /// 由 setup 阶段经 [`Self::set_app_handle`] 注入（builder 链构造时尚无 handle）。
    /// 每次 [`Self::ensure_core`] 创建 Core 时，用此句柄现场构造全部插件实例。
    app_handle: std::sync::OnceLock<tauri::AppHandle>,
    /// 工具消息注入通道（插件作为生产者 push，app 消费者统一处理）。
    /// 插件通过 [`Self::tool_injection_tx`] 获取 sender，直接 push `ToolInjection`。
    /// 消费者任务由 [`Self::start_tool_injection_consumer`] 启动。
    tool_injection_tx: tokio::sync::mpsc::UnboundedSender<ToolInjection>,
    /// 消费者 receiver（Option：take 出来启动消费者任务后变 None）。
    tool_injection_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<ToolInjection>>>,
}

/// 工具消息注入请求（插件 → app 消费者）。
pub struct ToolInjection {
    /// 注入到哪个 session（None = 当前活跃 session）。
    pub session_id: Option<String>,
    /// 注入的工具数据。
    pub tool: Box<dyn tiangong_core::agent_input::ToolInput>,
    /// 注入后是否需要刷新前端（emit run_snapshot）。
    pub refresh_frontend: bool,
}

#[derive(Debug, Clone)]
struct DraftSendClaim {
    revision: u64,
    attachment_paths: Vec<String>,
}

impl TiangongApp {
    /// 构造应用状态。`app_handle` 由 setup 阶段经 [`Self::set_app_handle`] 注入。
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        // 初始化 config 内存单例（从磁盘加载一次，后续读内存）。
        tiangong_config::registry::init();
        let core_config = tiangong_config::registry::config().to_core_config();
        let config = CoreConfigProvider::new(core_config);

        let (tool_injection_tx, tool_injection_rx) = tokio::sync::mpsc::unbounded_channel();

        // 构造 state：load_or_default 经 RuntimeEngine::new 注入 storage_root 到 core
        //（core 运行时持久化需要）。config 加载走自己的 dir，不依赖 core cell。
        let storage_root = tiangong_app_state::app_state::storage_root();
        let state = std::sync::Arc::new(AsyncMutex::new(
            tiangong_app_state::app_state::TiangongState::load_or_default(),
        ));
        let scheduler_context = std::sync::Arc::new(
            crate::scheduler::DesktopSchedulerContext::new(state.clone(), config.clone()),
        );

        Self {
            state,
            cores: Mutex::new(HashMap::new()),
            session_send_locks: Mutex::new(HashMap::new()),
            draft_update_locks: Mutex::new(HashMap::new()),
            draft_redirects: Mutex::new(HashMap::new()),
            delivered_draft_revisions: Mutex::new(HashMap::new()),
            draft_send_claims: Mutex::new(HashMap::new()),
            discarded_drafts: Mutex::new(HashSet::new()),
            active_session_epoch: AtomicU64::new(0),
            config,
            scheduler_context,
            plugin_dirty_sessions: Mutex::new(HashSet::new()),
            skill_plugin: std::sync::Arc::new(
                tiangong_plugin_skill::SkillPlugin::with_storage_root(storage_root.join("skills")),
            ),
            mcp_plugin: std::sync::Arc::new(tiangong_plugin_mcp::McpPlugin::with_storage_root(
                storage_root,
            )),
            embedded_server: Mutex::new(None),
            app_handle: std::sync::OnceLock::new(),
            tool_injection_tx,
            tool_injection_rx: Mutex::new(Some(tool_injection_rx)),
        }
    }

    /// 注入 Tauri 应用句柄（setup 阶段调用，仅一次）。
    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        let _ = self.app_handle.set(handle);
    }

    /// 获取工具消息注入 channel sender。
    ///
    /// 插件持有此 sender 后可直接投递 `ToolInjection`，无需经过 emit/listen 事件中转。
    /// 消费者任务由 [`Self::start_tool_injection_consumer`] 启动后统一处理。
    pub fn tool_injection_tx(&self) -> tokio::sync::mpsc::UnboundedSender<ToolInjection> {
        self.tool_injection_tx.clone()
    }

    /// 启动工具消息注入消费者任务（main.rs setup 阶段调用一次）。
    ///
    /// 循环接收插件 push 的 `ToolInjection`，统一处理注入到 session。
    /// 注入逻辑与 [`Self::inject_tool`] 相同，但支持指定 session_id 和前端刷新。
    pub fn start_tool_injection_consumer(&self, app_handle: tauri::AppHandle) {
        let rx = {
            let mut guard = self.tool_injection_rx.lock().unwrap();
            guard.take()
        };
        let Some(mut rx) = rx else {
            tracing::warn!("工具消息注入消费者已启动，跳过重复启动");
            return;
        };

        // 持有 Arc<state> 让消费者任务独立存活
        let state = self.state.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(req) = rx.recv().await {
                let session_id = match req.session_id {
                    Some(id) => id,
                    None => {
                        let guard = state.lock().await;
                        guard.active_session_id().to_string()
                    }
                };

                let tool_name = req.tool.tool_name().to_string();

                // 通过 app_handle 获取 TiangongApp
                let app_state = app_handle.state::<TiangongApp>();

                // core 不存在 → 自动恢复（ensure_core），保证 stream_tx 可用
                let core_exists = {
                    let cores = app_state.lock_cores();
                    cores.get(&session_id).is_some()
                };
                if !core_exists {
                    // 从 state 取 session 快照
                    let session_snapshot = {
                        let guard = state.lock().await;
                        guard
                            .sessions()
                            .iter()
                            .find(|s| s.id == session_id)
                            .cloned()
                    };
                    if let Some(session) = session_snapshot {
                        use std::sync::mpsc;
                        use tiangong_types::SessionStreamEvent;
                        let (stream_tx, stream_rx) = mpsc::channel::<SessionStreamEvent>();
                        let (_sid, _is_new) =
                            app_state.ensure_core(&session_id, session, stream_tx).await;
                        // 启动 stream_consumer 同步 worker session → TiangongState session
                        let cancel_flag = {
                            let cores = app_state.lock_cores();
                            cores.get(&session_id).map(|c| c.cancel_flag())
                        };
                        if let Some(cancel_flag) = cancel_flag {
                            crate::commands::start_stream_consumer(
                                app_handle.clone(),
                                session_id.clone(),
                                stream_rx,
                                cancel_flag,
                            );
                        }
                        tracing::info!(session_id, "消费者自动恢复 core");
                    } else {
                        tracing::warn!(session_id, "消费者无法恢复 core：session 不存在");
                        continue;
                    }
                }

                // core 存在 → deliver(Tool)，worker 通过 StreamEvent 处理注入
                let core_sent = {
                    let cores = app_state.lock_cores();
                    if let Some(core) = cores.get(&session_id) {
                        use tiangong_core::agent_input::{AgentInput, AgentInputKind};
                        let sent = core.deliver(AgentInputKind::Tool(req.tool)).is_ok();
                        drop(cores);
                        sent
                    } else {
                        false
                    }
                };

                if !core_sent {
                    tracing::warn!(session_id, tool_name, "deliver 失败（core 通道已关闭）");
                }

                tracing::debug!(session_id, tool_name, "工具消息注入完成");
            }
            tracing::info!("工具消息注入消费者任务结束");
        });
    }

    /// 同步工具消息注入（供需要同步返回值的场景，如 browser:events 的 ack 判断）。
    ///
    /// core 不存在时返回 false。大多数场景应通过 [`Self::tool_injection_tx`] push 到 channel，
    /// 消费者会自动 ensure_core 恢复 core 后注入。
    pub async fn inject_tool(&self, tool: Box<dyn tiangong_core::agent_input::ToolInput>) -> bool {
        let tool_name = tool.tool_name().to_string();
        let session_id = {
            let guard = self.state.lock().await;
            guard.active_session_id().to_string()
        };

        let cores = self.lock_cores();
        if let Some(core) = cores.get(&session_id) {
            use tiangong_core::agent_input::{AgentInput, AgentInputKind};
            tracing::info!(
                session_id,
                tool_name,
                stopped = core.is_stopped(),
                "注入工具消息 via deliver"
            );
            core.deliver(AgentInputKind::Tool(tool)).is_ok()
        } else {
            tracing::warn!(
                session_id,
                tool_name,
                "inject_tool: core 不存在，返回 false（应走 channel 消费者自动恢复）"
            );
            false
        }
    }

    fn lock_cores(&self) -> std::sync::MutexGuard<'_, HashMap<String, TiangongCore>> {
        match self.cores.lock() {
            Ok(guard) => guard,
            Err(err) => {
                warn!(error = %err, "cores 锁已污染，尝试恢复");
                err.into_inner()
            }
        }
    }

    pub fn session_send_lock(&self, session_id: &str) -> std::sync::Arc<AsyncMutex<()>> {
        let mut locks = match self.session_send_locks.lock() {
            Ok(guard) => guard,
            Err(err) => {
                warn!(error = %err, "session_send_locks 锁已污染，尝试恢复");
                err.into_inner()
            }
        };
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(AsyncMutex::new(())))
            .clone()
    }

    pub fn active_session_epoch(&self) -> u64 {
        self.active_session_epoch.load(Ordering::Acquire)
    }

    pub fn mark_active_session_changed(&self) -> u64 {
        self.active_session_epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn remove_session_send_lock(&self, session_id: &str) {
        // 不从锁表删除 Arc：若旧 guard/等待者仍存活，新请求创建另一把锁
        // 会直接破坏互斥。这些小锁保留到进程结束。
        let mut delivered = match self.delivered_draft_revisions.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        delivered.remove(session_id);
        drop(delivered);
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        claims.remove(session_id);
        drop(claims);
        let mut redirects = match self.draft_redirects.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        let discarded_aliases = redirects
            .iter()
            .filter(|(from, to)| from.as_str() == session_id || to.as_str() == session_id)
            .map(|(from, _)| from.clone())
            .collect::<Vec<_>>();
        redirects.retain(|from, to| from != session_id && to != session_id);
        drop(redirects);
        let mut discarded = match self.discarded_drafts.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        discarded.insert(session_id.to_string());
        discarded.extend(discarded_aliases);
    }

    pub fn resolve_draft_session_id(&self, session_id: &str) -> String {
        let redirects = match self.draft_redirects.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        let mut resolved = session_id.to_string();
        let mut seen = HashSet::new();
        while seen.insert(resolved.clone()) {
            let Some(next) = redirects.get(&resolved) else {
                break;
            };
            resolved = next.clone();
        }
        resolved
    }

    pub fn redirect_input_draft(&self, from_session_id: &str, to_session_id: &str) {
        if from_session_id == to_session_id {
            return;
        }
        let resolved_target = self.resolve_draft_session_id(to_session_id);
        let mut redirects = match self.draft_redirects.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        redirects.insert(from_session_id.to_string(), resolved_target.clone());
        for target in redirects.values_mut() {
            if target == from_session_id {
                *target = resolved_target.clone();
            }
        }
        drop(redirects);
        let mut delivered = match self.delivered_draft_revisions.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if let Some(from_revision) = delivered.remove(from_session_id) {
            delivered
                .entry(resolved_target.clone())
                .and_modify(|current| *current = (*current).max(from_revision))
                .or_insert(from_revision);
        }
        drop(delivered);
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if let Some(from_claim) = claims.remove(from_session_id) {
            let should_replace = claims
                .get(&resolved_target)
                .map(|target| target.revision <= from_claim.revision)
                .unwrap_or(true);
            if should_replace {
                claims.insert(resolved_target, from_claim);
            }
        }
        drop(claims);
        let mut discarded = match self.discarded_drafts.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        discarded.remove(from_session_id);
        discarded.remove(to_session_id);
    }

    pub fn mark_draft_discarded(&self, session_id: &str) {
        let mut discarded = match self.discarded_drafts.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        discarded.insert(session_id.to_string());
    }

    pub fn draft_was_discarded(&self, session_id: &str) -> bool {
        let discarded = match self.discarded_drafts.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        discarded.contains(session_id)
    }

    pub fn draft_revision_was_delivered(&self, session_id: &str, revision: u64) -> bool {
        let delivered = match self.delivered_draft_revisions.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        delivered
            .get(session_id)
            .is_some_and(|current| revision <= *current)
    }

    pub fn mark_draft_revision_delivered(&self, session_id: &str, revision: u64) {
        let mut delivered = match self.delivered_draft_revisions.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        delivered
            .entry(session_id.to_string())
            .and_modify(|current| *current = (*current).max(revision))
            .or_insert(revision);
    }

    /// 冻结一版已归档草稿用于发送。返回被更新 revision 替换的旧租约路径。
    pub fn register_draft_send_claim(
        &self,
        session_id: &str,
        revision: u64,
        attachment_paths: Vec<String>,
    ) -> Result<Vec<String>, String> {
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if let Some(existing) = claims.get(session_id) {
            if existing.revision > revision {
                return Err("该草稿已有更新版本正在准备发送".to_string());
            }
            if existing.revision == revision {
                if existing.attachment_paths == attachment_paths {
                    return Ok(Vec::new());
                }
                return Err("同一草稿版本的附件快照不一致".to_string());
            }
        }
        let replaced = claims
            .insert(
                session_id.to_string(),
                DraftSendClaim {
                    revision,
                    attachment_paths,
                },
            )
            .map(|claim| claim.attachment_paths)
            .unwrap_or_default();
        Ok(replaced)
    }

    pub fn has_draft_send_claim(&self, session_id: &str, revision: u64) -> bool {
        let claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        claims
            .get(session_id)
            .is_some_and(|claim| claim.revision == revision)
    }

    pub fn release_draft_send_claim(&self, session_id: &str, revision: u64) -> Vec<String> {
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if claims
            .get(session_id)
            .is_some_and(|claim| claim.revision == revision)
        {
            return claims
                .remove(session_id)
                .map(|claim| claim.attachment_paths)
                .unwrap_or_default();
        }
        Vec::new()
    }

    pub fn release_any_draft_send_claim(&self, session_id: &str) -> Vec<String> {
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        claims
            .remove(session_id)
            .map(|claim| claim.attachment_paths)
            .unwrap_or_default()
    }

    pub fn claimed_draft_attachment_paths(&self) -> HashSet<String> {
        let claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        claims
            .values()
            .flat_map(|claim| claim.attachment_paths.iter().cloned())
            .collect()
    }

    pub fn draft_update_lock(&self, session_id: &str) -> std::sync::Arc<AsyncMutex<()>> {
        let mut locks = match self.draft_update_locks.lock() {
            Ok(guard) => guard,
            Err(err) => {
                warn!(error = %err, "draft_update_locks 锁已污染，尝试恢复");
                err.into_inner()
            }
        };
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn lock_embedded_server(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<tiangong_server::EmbeddedServerHandle>> {
        match self.embedded_server.lock() {
            Ok(guard) => guard,
            Err(err) => {
                warn!(error = %err, "embedded_server 锁已污染，尝试恢复");
                err.into_inner()
            }
        }
    }

    pub async fn sync_core_config_from_state(&self) -> Result<(), String> {
        let base = self.config.snapshot();
        // old_sig 从 registry 旧值算（set_models 之前），new_sig 从 app-state 新值算。
        let old_sig =
            tiangong_config::registry::plugin_set_signature(&tiangong_config::registry::models());
        let (template, session_configs, new_sig) = self
            .with_state_read(|core_state| {
                let new_models = core_state.models_config().clone();
                let new_sig = tiangong_config::registry::plugin_set_signature(&new_models);
                // 同步 app-state 的最新 models 到 config 内存单例。
                tiangong_config::registry::set_models(new_models);
                let template = core_state.build_core_config_for_session_from_base(&base, "");
                let session_configs = core_state
                    .sessions()
                    .iter()
                    .map(|session| {
                        (
                            session.id.clone(),
                            core_state.build_core_config_for_session_from_base(&base, &session.id),
                        )
                    })
                    .collect::<HashMap<_, _>>();
                Ok((template, session_configs, new_sig))
            })
            .await?;
        let plugin_set_changed = old_sig != new_sig;
        // 这份 provider 只作全局模板和新 Core 构造辅助，不承载任一会话的
        // trust/reasoning 覆盖。已存在 Core 使用各自独立的 provider。
        self.config.replace(template);
        let cores = self.lock_cores();
        if plugin_set_changed {
            // 能力集合变化（新增/删除）：plugin 列表构造时固定，无法热更新。
            // 标记 dirty，下次 ensure_core 时移除旧 core 重建（不打断当前 turn）。
            for session_id in cores.keys().cloned().collect::<Vec<_>>() {
                self.plugin_dirty_sessions
                    .lock()
                    .map(|mut g| g.insert(session_id))
                    .ok();
            }
        } else {
            // 仅 endpoint 或会话配置变化：按 session 替换独立 provider，
            // 避免并行会话互相覆盖 trust/reasoning。
            for (session_id, core) in cores.iter() {
                if let Some(config) = session_configs.get(session_id) {
                    let _ = core.replace_config(config.clone());
                    core.set_trust_mode(config.trust_mode);
                }
            }
        }
        Ok(())
    }

    pub async fn with_state<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut tiangong_app_state::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        let mut guard = self.state.lock().await;
        f(&mut guard).map_err(|e| e.to_string())
    }

    pub async fn with_state_read<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&tiangong_app_state::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        let guard = self.state.lock().await;
        f(&guard).map_err(|e| e.to_string())
    }

    /// 获取或创建会话对应的 TiangongCore
    ///
    /// 如果 core 已存在（多轮对话），直接复用。
    /// stream_tx 只在创建新 core 时使用。
    pub async fn ensure_core(
        &self,
        session_id: &str,
        mut session: tiangong_core::session::Session,
        stream_tx: std::sync::mpsc::Sender<tiangong_types::SessionStreamEvent>,
    ) -> (String, bool) {
        // Invariant: 无效 CWD 的会话不会加载到 Core 生命周期。调用方在加载会话前
        // 已过滤掉 cwd 为无效目录的会话，因此插件可以假设 session.cwd 要么为空
        //（普通聊天会话）要么是有效工作区目录。
        //
        // Invariant: 同一 session 的 ensure_core 调用由上层业务串行化（Tauri 命令
        // 经 session 级互斥 / 前端单消息流保证），不会并发为同一 session 创建 Core。
        // 因此这里可以在 await 初始化 memory handle 后直接创建并插入。
        // 如未来允许同 session 并发入口，需要在 await 后增加二次检查。

        let base = self.config.snapshot();
        let (session_config, session_pending) = {
            let core_state = self.state.lock().await;
            (
                core_state.build_core_config_for_session_from_base(&base, session_id),
                core_state.has_active_turn_for(session_id),
            )
        };

        // 1. 先检查是否已有 core（持有锁期间不做 async 操作）
        let retired_core = {
            let mut cores = self.lock_cores();
            // 插件集合变化（能力新增/删除）时移除旧 core，用最新 models 重建。
            let dirty = self
                .plugin_dirty_sessions
                .lock()
                .map(|mut g| g.remove(session_id))
                .unwrap_or(false);
            if dirty {
                if session_pending {
                    if let Some(core) = cores.get(session_id) {
                        if !core.is_stopped() {
                            self.plugin_dirty_sessions
                                .lock()
                                .map(|mut dirty| dirty.insert(session_id.to_string()))
                                .ok();
                            let _ = core.replace_config(session_config.clone());
                            core.set_trust_mode(session.trust_mode);
                            return (session_id.to_string(), false);
                        }
                    }
                }
                let retired = cores.remove(session_id);
                if retired.is_some() {
                    tracing::info!(session_id, "插件集合变化，移除旧 core 待重建");
                }
                retired
            } else if let Some(core) = cores.get(session_id) {
                if !core.is_stopped() {
                    let _ = core.replace_config(session_config.clone());
                    let _ = core.deliver(AgentInputKind::update_cwd(session.cwd.clone()));
                    core.set_trust_mode(session.trust_mode);
                    return (session_id.to_string(), false); // 已存在，复用
                }
                warn!(session_id, "移除已停止的 TiangongCore");
                cores.remove(session_id)
            } else {
                None
            }
        };

        if let Some(core) = retired_core {
            let desired_title = session.title.clone();
            let desired_cwd = session.cwd.clone();
            let desired_trust_mode = session.trust_mode;
            match tokio::task::spawn_blocking(move || core.into_session()).await {
                Ok(Ok(mut final_session)) => {
                    final_session.title = desired_title;
                    final_session.cwd = desired_cwd;
                    final_session.trust_mode = desired_trust_mode;
                    final_session.persist_to_disk();
                    session = final_session;
                }
                Ok(Err(error)) => {
                    warn!(%error, session_id, "插件重建前关闭旧 Core 失败");
                }
                Err(error) => {
                    warn!(%error, session_id, "插件重建前等待旧 Core 失败");
                }
            }
        }

        // 2. 初始化 Memory Handle（async，不持有 cores 锁）。
        let memory_handle = tiangong_memory::registry::init_memory_handle_for_process(
            self.config.generation(),
            tiangong_memory::ProcessType::Gui,
        )
        .await;

        // 3. 现场构造全部插件实例（per-Core 独立，隔离 per-session 状态）。
        let mut plugins: Vec<std::sync::Arc<dyn tiangong_core::core::Plugin>> = Vec::new();
        let Some(app_handle) = self.app_handle.get() else {
            panic!("TiangongApp.app_handle 未注入，set_app_handle 应在 setup 阶段调用");
        };
        if let Some(browser) = tiangong_plugin_browser::build_plugin(app_handle) {
            plugins.push(browser);
        } else {
            warn!("浏览器插件构造失败（Tauri state 未就绪），浏览器能力将缺失");
        }
        if let Some(terminal) = tiangong_plugin_terminal::build_plugin(app_handle) {
            plugins.push(terminal);
        } else {
            warn!("终端插件构造失败（Tauri state 未就绪），终端能力将缺失");
        }
        plugins.push(tiangong_plugin_fs::build_plugin());
        plugins.push(tiangong_plugin_index::build_plugin());
        // app 层判断是否注册各能力插件，经 llm 路由解析端点后构造注入。
        // models 从 config 内存单例读取（sync_core_config_from_state 时已同步）。
        use tiangong_llm::{ModelCapability, ModelEndpoint, SingleProviderClient};
        let models = tiangong_config::registry::models();
        let resolve_ep = |cap: ModelCapability| {
            models
                .resolve_for_capability(cap)
                .map(ModelEndpoint::from_resolved)
        };
        if let Some(ep) = resolve_ep(ModelCapability::ImageGeneration) {
            plugins.push(tiangong_plugin_generate_image::build_plugin(ep));
        }
        if let Some(ep) = resolve_ep(ModelCapability::VideoGeneration) {
            plugins.push(tiangong_plugin_generate_video::build_plugin(ep));
        }
        if let Some(ep) = resolve_ep(ModelCapability::Tts) {
            plugins.push(tiangong_plugin_text_to_speech::build_plugin(ep));
        }
        if let Some(ep) = resolve_ep(ModelCapability::Stt) {
            plugins.push(tiangong_plugin_speech_to_text::build_plugin(ep));
        }
        plugins.push(tiangong_plugin_memory::build_plugin(memory_handle));
        plugins.push(tiangong_plugin_scheduler::build_plugin());
        plugins.push(tiangong_plugin_task::build_plugin());
        if models.has_capability(ModelCapability::Multimodal) && !models.chat_is_multimodal() {
            if let Some(client) =
                resolve_ep(ModelCapability::Multimodal).map(SingleProviderClient::new)
            {
                plugins.push(tiangong_plugin_analyze_attachment::build_plugin(client));
            }
        }
        // Skill 插件：dual-ownership——core 拿 clone 做 LLM 工具（get_skill_detail），
        // app 侧经 self.skill_plugin 做管理（remove/set_enabled/refresh/gc/doctor）。
        plugins.push(self.skill_plugin.clone());
        // MCP 插件：dual-ownership——core 拿 clone 做 LLM 工具（动态 MCP 工具），
        // app 侧经 self.mcp_plugin 做管理（register/update/remove/set_enabled/probe）。
        plugins.push(self.mcp_plugin.clone());
        // Agent Team 插件：子 Agent 管理 + 文件锁工具（issue #200）。
        plugins.push(tiangong_plugin_agent_team::build_plugin());

        // 4. 创建 Core 并插入（重新拿锁）。
        let core = TiangongCore::builder()
            .config(CoreConfigProvider::new(session_config))
            .session(session)
            .event_sender(stream_tx)
            .plugins(plugins)
            .storage(tiangong_core::core::CoreStorageLocation::new(
                tiangong_app_state::app_state::storage_root(),
            ))
            .build()
            .expect("Builder 必填字段已齐");
        let id = core.session_id().to_string();
        {
            let mut cores = self.lock_cores();
            // Memory/plugin 初始化包含 await；期间另一个入口可能已为同一会话创建 Core。
            // 二次检查避免后创建者覆盖正在运行的实例。
            if cores
                .get(session_id)
                .is_some_and(|existing| !existing.is_stopped())
            {
                return (session_id.to_string(), false);
            }
            cores.insert(id.clone(), core);
        }
        (id, true) // 新创建
    }

    /// 向指定会话的 core 发送消息
    pub fn send_to_core(&self, session_id: &str, content: String) -> bool {
        self.send_to_core_with_id(session_id, content, None)
    }

    /// 向指定会话的 core 发送带固定消息 ID 的消息
    pub fn send_to_core_with_id(
        &self,
        session_id: &str,
        content: String,
        message_id: Option<String>,
    ) -> bool {
        let mut cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            let sent = if let Some(message_id) = message_id {
                core.deliver(AgentInputKind::prepared_with_id(
                    message_id,
                    vec![tiangong_types::ContentBlock::text(content)],
                ))
                .is_ok()
            } else {
                core.deliver(AgentInputKind::message(content)).is_ok()
            };
            if !sent {
                warn!(session_id, "TiangongCore 命令通道已关闭，移除僵尸 core");
                cores.remove(session_id);
            }
            sent
        } else {
            false
        }
    }

    /// 取回 core 的 session（消费 core，用于持久化或切换会话）
    pub fn take_core(&self, session_id: &str) -> Option<TiangongCore> {
        let mut cores = self.lock_cores();
        cores.remove(session_id)
    }

    /// 仅移除已经停止的当前 Core。旧消费者晚到的 EOF 不会误删同会话的新实例。
    pub fn remove_stopped_core(&self, session_id: &str) -> bool {
        let mut cores = self.lock_cores();
        if cores.get(session_id).is_some_and(TiangongCore::is_stopped) {
            cores.remove(session_id);
            true
        } else {
            false
        }
    }

    pub fn is_current_core_instance(
        &self,
        session_id: &str,
        cancel_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> bool {
        self.lock_cores()
            .get(session_id)
            .map(|core| core.cancel_flag())
            .is_some_and(|current| std::sync::Arc::ptr_eq(&current, cancel_flag))
    }

    /// 取消指定会话的执行
    pub fn cancel_core(&self, session_id: &str) {
        let cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            let _ = core.deliver(AgentInputKind::cancel());
        }
    }

    /// 取消指定会话中某个 Agent 的当前执行
    pub fn cancel_agent_core(&self, session_id: &str, role: String) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.deliver(AgentInputKind::cancel_agent(role)).is_ok())
            .unwrap_or(false)
    }

    /// 向指定会话的 core 发送审批响应
    pub fn respond_approval_to_core(&self, session_id: &str, request_id: String, approved: bool) {
        let cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            let _ = core.deliver(AgentInputKind::approval(request_id, approved));
        }
    }

    /// 设置所有活跃 core 的信任模式（全局生效）
    pub fn set_all_cores_trust_mode(&self, mode: tiangong_core::permission::TrustMode) {
        let cores = self.lock_cores();
        for core in cores.values() {
            core.set_trust_mode(mode);
        }
    }

    /// 设置指定会话 core 的信任模式（实时生效）
    pub fn set_core_trust_mode(
        &self,
        session_id: &str,
        mode: tiangong_core::permission::TrustMode,
    ) {
        let cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            core.set_trust_mode(mode);
        }
    }

    /// 检查 session 是否有活跃 core
    pub fn is_session_executing(&self, session_id: &str) -> bool {
        let cores = self.lock_cores();
        cores.contains_key(session_id)
    }

    /// 手动触发上下文压缩
    pub fn compress_context_core(&self, session_id: &str) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.deliver(AgentInputKind::compress_context()).is_ok())
            .unwrap_or(false)
    }

    /// 清理上下文（重置 LLM 上下文到初始 system prompt）
    pub fn reset_context_core(&self, session_id: &str) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.deliver(AgentInputKind::reset_context()).is_ok())
            .unwrap_or(false)
    }

    /// 启动嵌入式 Server（共享 app 的 state 和 config）
    pub fn start_embedded_server(
        &self,
        host: &str,
        port: u16,
        token: Option<String>,
    ) -> Result<(), String> {
        let mut guard = self.lock_embedded_server();
        if guard.is_some() {
            return Err("Server 已在运行".to_string());
        }
        let handle = tiangong_server::run_embedded(
            host,
            port,
            token,
            self.state.clone(),
            self.config.clone(),
        )
        .map_err(|e| e.to_string())?;
        *guard = Some(handle);
        Ok(())
    }

    /// 停止嵌入式 Server
    pub fn stop_embedded_server(&self) -> Result<(), String> {
        let mut guard = self.lock_embedded_server();
        if let Some(mut handle) = guard.take() {
            handle.stop();
            Ok(())
        } else {
            Err("Server 未在运行".to_string())
        }
    }

    /// 检查嵌入式 Server 是否在运行
    pub fn is_embedded_server_running(&self) -> bool {
        let guard = self.lock_embedded_server();
        guard.is_some()
    }

    /// 创建调度器执行上下文（用于 Desktop 端独立执行定时任务）
    pub fn create_scheduler_context(
        &self,
    ) -> std::sync::Arc<dyn tiangong_scheduler::executor::SchedulerContext> {
        self.scheduler_context.clone()
    }
}
