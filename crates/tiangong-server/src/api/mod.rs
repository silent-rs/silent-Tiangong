mod bots;
mod chat;
mod health;
mod jobs;
mod mcp;
mod messages;
mod server_ctrl;
mod sessions;
mod skills;
mod types;
mod webhook;
pub mod ws;

use std::sync::Arc;

use silent::prelude::*;
use tiangong_app_state::app_state::TiangongState;
use tiangong_config::CoreConfigProvider;
use tiangong_scheduler::executor::SchedulerContext;
use tokio::sync::Mutex;

use crate::remote::backend::ServerCoreBackend;
use crate::remote::core::ServerCoreManager;
use crate::remote::event::EventBus;
use crate::remote::router::MessageRouter;
use crate::scheduler::context::ServerSchedulerContext;

/// 共享应用状态类型
pub type SharedState = Arc<Mutex<TiangongState>>;

/// Bot 回连 Server 所需的实际连接信息（issue #286 review 问题2）。
///
/// 由 Server 启动时的实际参数（host/port/token）填充，所有 bot 启动路径
/// （start_enabled / start / restart / 升级恢复）统一从本结构生成 extra_env，
/// 不再重新读 server.json——避免命令行参数与持久化配置不一致导致 bot 连不上。
#[derive(Debug, Clone)]
pub struct BotConnectInfo {
    /// 规范化后的可连接 host（通配地址 0.0.0.0/::/空 → 127.0.0.1）。
    pub connect_host: String,
    pub port: u16,
    pub token: Option<String>,
}

impl BotConnectInfo {
    /// 由原始监听 host/port/token 构造，host 经 connect_host 规范化。
    pub fn new(host: &str, port: u16, token: Option<String>) -> Self {
        Self {
            connect_host: connect_host(host),
            port,
            token,
        }
    }

    /// 生成 bot 回连所需的 extra_env（TIANGONG_URL / TIANGONG_TOKEN）。
    pub fn to_bot_env(&self) -> std::collections::BTreeMap<String, String> {
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "TIANGONG_URL".to_string(),
            format!("http://{}:{}", self.connect_host, self.port),
        );
        if let Some(t) = &self.token {
            env.insert("TIANGONG_TOKEN".to_string(), t.clone());
        }
        env
    }
}

/// 规范化监听地址为可连接地址（通配/空 → 127.0.0.1）。
pub fn connect_host(host: &str) -> String {
    match host.trim() {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1".to_string(),
        value => value.to_string(),
    }
}

/// Server 共享上下文：统一持有应用状态、Core 运行时与消息路由器
#[derive(Clone)]
pub struct ServerAppContext {
    pub state: SharedState,
    pub config: CoreConfigProvider,
    pub core_backend: Arc<dyn ServerCoreBackend>,
    pub scheduler_context: Arc<dyn SchedulerContext>,
    pub router: Arc<MessageRouter>,
    /// MCP 管理插件共享句柄：API 管理（register/remove/...）与 core 注册使用同一实例，
    /// 避免管理实例与运行实例状态分叉（对齐 CLI/Tauri 的 dual-ownership）。
    pub mcp_plugin: Arc<tiangong_plugin_mcp::McpPlugin>,
    /// Bot 配置存储（读写 bots.json）。独立 Server 自建；嵌入式复用 Desktop 注入实例。
    pub bot_store: Arc<tiangong_bots::BotStore>,
    /// Bot 运行时——制品下载、进程监督与启停（issue #286 阶段 2，Server 接管生命周期）。
    pub bot_runtime: Arc<tiangong_bots::BotRuntime>,
    /// Bot 回连 Server 的实际连接信息（review 问题2）：所有 bot 启动路径统一使用。
    pub bot_connect: BotConnectInfo,
}

impl ServerAppContext {
    pub fn new(
        state: SharedState,
        core_manager: tiangong_app_state::app_state::CoreManager,
        event_bus: Arc<EventBus>,
        storage_root: std::path::PathBuf,
        bot_connect: BotConnectInfo,
    ) -> Self {
        let config = core_manager.config().clone();
        let mcp_plugin = Arc::new(tiangong_plugin_mcp::McpPlugin::with_storage_root(
            storage_root.clone(),
        ));
        // 工作区索引单例（issue #259）：跨 Core 共享底层索引缓存与扫描状态。
        // 构造失败时降级为独立实例（plugin 内部仍会兜底自建），不阻断启动。
        let index_manager = tiangong_plugin_index::shared_index_manager().unwrap_or_else(|e| {
            tracing::warn!("共享 IndexManager 初始化失败，降级独立实例: {e}");
            Arc::new(
                tiangong_plugin_index::IndexManager::new().expect("IndexManager 初始化兜底失败"),
            )
        });
        let cores = Arc::new(ServerCoreManager::new(
            state.clone(),
            core_manager,
            event_bus.clone(),
            mcp_plugin.clone(),
            index_manager,
        ));
        let core_backend: Arc<dyn ServerCoreBackend> = cores;
        let scheduler_context: Arc<dyn SchedulerContext> = Arc::new(ServerSchedulerContext {
            state: state.clone(),
            core_backend: core_backend.clone(),
        });
        // Bot 管理句柄（issue #286 阶段 2）：独立 Server 自建，对齐 Desktop app.rs。
        // 构造失败不阻断 Server 启动（bot 相关 API 会返回错误）。
        let bot_store = Arc::new(
            tiangong_bots::BotStore::with_storage_root(storage_root.clone()).unwrap_or_else(|e| {
                tracing::warn!("BotStore 初始化失败，bot 管理 API 将不可用: {e}");
                tiangong_bots::BotStore::default()
            }),
        );
        let bot_runtime = Arc::new(
            tiangong_bots::BotRuntime::new(bot_store.clone())
                .unwrap_or_else(|e| panic!("独立 Server 构造 BotRuntime 失败: {e}")),
        );
        Self::with_backend(
            state,
            config,
            event_bus,
            core_backend,
            scheduler_context,
            mcp_plugin,
            bot_store,
            bot_runtime,
            bot_connect,
        )
    }

    /// 使用宿主提供的 Core 与调度上下文构建 Server。
    ///
    /// Desktop 内嵌模式必须走此入口，确保 HTTP 与桌面 UI 共用同一套 Core 生命周期。
    #[allow(clippy::too_many_arguments)]
    pub fn with_backend(
        state: SharedState,
        config: CoreConfigProvider,
        event_bus: Arc<EventBus>,
        core_backend: Arc<dyn ServerCoreBackend>,
        scheduler_context: Arc<dyn SchedulerContext>,
        mcp_plugin: Arc<tiangong_plugin_mcp::McpPlugin>,
        bot_store: Arc<tiangong_bots::BotStore>,
        bot_runtime: Arc<tiangong_bots::BotRuntime>,
        bot_connect: BotConnectInfo,
    ) -> Self {
        let router = Arc::new(MessageRouter::new(
            state.clone(),
            event_bus,
            core_backend.clone(),
        ));
        Self {
            state,
            config,
            core_backend,
            scheduler_context,
            router,
            mcp_plugin,
            bot_store,
            bot_runtime,
            bot_connect,
        }
    }

    pub async fn sync_core_config_from_state(&self) {
        if let Err(error) = self.core_backend.sync_config_from_state().await {
            tracing::warn!(%error, "同步 Core 配置失败");
        }
    }
}

/// 共享 Server 上下文
pub type SharedAppContext = Arc<ServerAppContext>;

/// 认证 Token 包装（用于注入到 Configs 中）
#[derive(Clone, Debug)]
pub struct AuthToken(pub Option<String>);

/// 构建完整的 API 路由树，通过 Configs 注入共享状态和 Token
#[allow(deprecated)]
pub fn build_routes(
    app: SharedAppContext,
    token: Option<String>,
    event_bus: Arc<EventBus>,
) -> (Route, Configs) {
    let mut configs = Configs::default();
    configs.insert(app);
    configs.insert(AuthToken(token));
    configs.insert(ws::SharedEventBus(event_bus));

    let route = Route::new("api/v1")
        .append(Route::new("health").get(health::health_check))
        .append(Route::new("chat").post(chat::chat))
        .append(Route::new("messages").post(messages::post_message))
        .append(
            Route::new("sessions").get(sessions::list_sessions).append(
                Route::new("<id>")
                    .get(sessions::get_session)
                    .append(Route::new("cost").get(sessions::get_session_cost))
                    .delete(sessions::delete_session),
            ),
        )
        .append(Route::new("mcp").get(mcp::list_mcp))
        .append(
            Route::new("bots")
                .get(bots::list_bots)
                .post(bots::register_bot)
                .append(Route::new("available").get(bots::list_available))
                .append(Route::new("install").post(bots::install_bot))
                .append(
                    Route::new("check-update")
                        .append(Route::new("<artifact_id>").get(bots::check_update)),
                )
                .append(
                    Route::new("<id>")
                        .get(bots::get_bot)
                        .delete(bots::delete_bot)
                        .append(Route::new("health").get(bots::get_bot_health))
                        .append(Route::new("logs").get(bots::get_bot_logs))
                        .append(Route::new("schema").get(bots::get_bot_schema))
                        .append(Route::new("config").put(bots::update_bot_config))
                        .append(Route::new("start").post(bots::start_bot))
                        .append(Route::new("stop").post(bots::stop_bot))
                        .append(Route::new("restart").post(bots::restart_bot))
                        .append(Route::new("upgrade").post(bots::upgrade_bot))
                        .append(
                            Route::new("provision")
                                .append(Route::new("begin").post(bots::provision_begin))
                                .append(Route::new("poll").post(bots::provision_poll)),
                        ),
                ),
        )
        .append(Route::new("skills").get(skills::list_skills))
        .append(
            Route::new("jobs")
                .get(jobs::list_jobs)
                .post(jobs::create_job)
                .append(
                    Route::new("<id>")
                        .get(jobs::get_job)
                        .put(jobs::update_job)
                        .delete(jobs::delete_job)
                        .append(Route::new("runs").get(jobs::list_job_runs))
                        .append(Route::new("trigger").post(jobs::trigger_job)),
                ),
        )
        .append(
            Route::new("webhooks")
                .get(webhook::list_webhooks)
                .post(webhook::create_webhook)
                .append(
                    Route::new("<id>")
                        .get(webhook::get_webhook)
                        .put(webhook::update_webhook)
                        .delete(webhook::delete_webhook)
                        .append(Route::new("runs").get(webhook::list_webhook_runs))
                        .append(Route::new("trigger").post(webhook::trigger_webhook))
                        .append(Route::new("invoke").post(webhook::invoke_webhook)),
                ),
        )
        .append(Route::new("server/shutdown").post(server_ctrl::shutdown))
        .append(ws::ws_route());

    (route, configs)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use anyhow::{Result, anyhow};
    use async_trait::async_trait;
    use tiangong_types::{
        IncomingMessage, MediaAsset, MessageContent, OutgoingMessage, RemoteRole,
    };

    use super::*;
    use crate::remote::backend::CoreBackendKind;

    struct EmbeddedBackend {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ServerCoreBackend for EmbeddedBackend {
        fn kind(&self) -> CoreBackendKind {
            CoreBackendKind::EmbeddedHost
        }

        async fn send_connector_message_and_wait(
            &self,
            _connector: &str,
            channel_id: &str,
            _content: String,
            _message_id: Option<String>,
            _media: Vec<MediaAsset>,
        ) -> Result<(String, OutgoingMessage)> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok((
                channel_id.to_string(),
                OutgoingMessage {
                    content: MessageContent::Text("embedded reply".to_string()),
                    attachments: Vec::new(),
                    reply_to: None,
                },
            ))
        }

        async fn send_message_and_wait(
            &self,
            _session_id: &str,
            _content: String,
            _message_id: Option<String>,
            _media: Vec<MediaAsset>,
        ) -> Result<(String, OutgoingMessage)> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(anyhow!("not used"))
        }

        async fn delete_session(&self, _session_id: &str) -> Result<bool> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(anyhow!("not used"))
        }

        async fn sync_config_from_state(&self) -> Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(anyhow!("not used"))
        }
    }

    struct EmbeddedScheduler {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SchedulerContext for EmbeddedScheduler {
        async fn send_message(&self, _session_id: &str, _content: String) -> Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(anyhow!("not used"))
        }

        async fn resolve_session_id(
            &self,
            _requested_session_id: Option<&str>,
        ) -> Result<(String, bool)> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(anyhow!("not used"))
        }
    }

    #[tokio::test]
    async fn embedded_messages_use_host_backend_without_starting_another_scheduler() {
        let _storage_guard = crate::remote::core::test_support::STORAGE_TEST_LOCK
            .lock()
            .await;
        let temp = tempfile::tempdir().unwrap();
        let _home_guard =
            crate::remote::core::test_support::TestHomeGuard::new(&temp.path().join("home"));
        let config = CoreConfigProvider::new(tiangong_config::CoreConfig::default());
        let state = Arc::new(Mutex::new(
            tiangong_app_state::app_state::TiangongState::new(),
        ));
        let event_bus = Arc::new(EventBus::default());
        let backend_calls = Arc::new(AtomicUsize::new(0));
        let scheduler_calls = Arc::new(AtomicUsize::new(0));
        let backend: Arc<dyn ServerCoreBackend> = Arc::new(EmbeddedBackend {
            calls: backend_calls.clone(),
        });
        let scheduler: Arc<dyn SchedulerContext> = Arc::new(EmbeddedScheduler {
            calls: scheduler_calls.clone(),
        });
        let mcp_plugin = Arc::new(tiangong_plugin_mcp::McpPlugin::with_storage_root(
            temp.path().to_path_buf(),
        ));
        let bot_store = Arc::new(
            tiangong_bots::BotStore::with_config_path(temp.path().join("bots.json"))
                .expect("test bot store"),
        );
        let bot_runtime =
            Arc::new(tiangong_bots::BotRuntime::new(bot_store.clone()).expect("test bot runtime"));

        let context = ServerAppContext::with_backend(
            state,
            config,
            event_bus,
            backend,
            scheduler,
            mcp_plugin,
            bot_store,
            bot_runtime,
            BotConnectInfo::new("127.0.0.1", 8080, None),
        );

        assert_eq!(context.core_backend.kind(), CoreBackendKind::EmbeddedHost);
        assert_eq!(backend_calls.load(Ordering::Relaxed), 0);
        assert_eq!(scheduler_calls.load(Ordering::Relaxed), 0);

        let outgoing = context
            .router
            .handle_incoming(IncomingMessage {
                id: "stable-message".to_string(),
                connector: "embedded-test".to_string(),
                channel_id: "desktop-session".to_string(),
                sender_id: "test".to_string(),
                sender_role: RemoteRole::Controller,
                content: MessageContent::Text("hello".to_string()),
                media: Vec::new(),
                reply_to: None,
                timestamp: tiangong_core::session::now_text(),
            })
            .await
            .unwrap();
        assert!(matches!(
            outgoing.content,
            MessageContent::Text(ref text) if text == "embedded reply"
        ));
        assert_eq!(backend_calls.load(Ordering::Relaxed), 1);
        assert_eq!(scheduler_calls.load(Ordering::Relaxed), 0);
    }
}
