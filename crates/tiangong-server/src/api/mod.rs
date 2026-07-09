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
use tokio::sync::Mutex;

use crate::remote::core::ServerCoreManager;
use crate::remote::event::EventBus;
use crate::remote::router::MessageRouter;

/// 共享应用状态类型
pub type SharedState = Arc<Mutex<TiangongState>>;

/// Server 共享上下文：统一持有应用状态、Core 运行时与消息路由器
#[derive(Clone)]
pub struct ServerAppContext {
    pub state: SharedState,
    pub config: CoreConfigProvider,
    pub cores: Arc<ServerCoreManager>,
    pub router: Arc<MessageRouter>,
    /// MCP 管理插件共享句柄：API 管理（register/remove/...）与 core 注册使用同一实例，
    /// 避免管理实例与运行实例状态分叉（对齐 CLI/Tauri 的 dual-ownership）。
    pub mcp_plugin: Arc<tiangong_plugin_mcp::McpPlugin>,
}

impl ServerAppContext {
    pub fn new(state: SharedState, config: CoreConfigProvider, event_bus: Arc<EventBus>) -> Self {
        // storage_root 由 app-state 统一计算；plugin 由 app 注入同一根目录，
        // 避免各自重复解析 ~/.tiangong。
        let storage_root = tiangong_app_state::app_state::storage_root();
        let mcp_plugin = Arc::new(tiangong_plugin_mcp::McpPlugin::with_storage_root(
            storage_root,
        ));
        let cores = Arc::new(ServerCoreManager::new(
            state.clone(),
            config.clone(),
            event_bus.clone(),
            mcp_plugin.clone(),
        ));
        let router = Arc::new(MessageRouter::new(state.clone(), event_bus, cores.clone()));
        Self {
            state,
            config,
            cores,
            router,
            mcp_plugin,
        }
    }

    pub async fn sync_core_config_from_state(&self) {
        let base = self.config.snapshot();
        let next = {
            let state = self.state.lock().await;
            let mut next = state.build_core_config_from_base(&base);
            next.trust_mode = tiangong_core::permission::TrustMode::FullTrust;
            next
        };
        self.config.replace(next);
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
            Route::new("sessions")
                .get(sessions::list_sessions)
                .post(sessions::create_session)
                .append(
                    Route::new("<id>")
                        .get(sessions::get_session)
                        .append(Route::new("cost").get(sessions::get_session_cost))
                        .delete(sessions::delete_session),
                ),
        )
        .append(Route::new("mcp").get(mcp::list_mcp))
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
