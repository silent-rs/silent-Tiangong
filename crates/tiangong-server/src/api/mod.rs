mod chat;
mod health;
mod mcp;
mod server_ctrl;
mod sessions;
mod skills;
mod types;
pub mod ws;

use std::sync::Arc;

use silent::prelude::*;
use tiangong_config::CoreConfigProvider;
use tiangong_core::app_state::TiangongState;
use tokio::sync::Mutex;

use crate::remote::core::ServerCoreManager;
use crate::remote::event::EventBus;
use crate::remote::router::MessageRouter;

/// 共享应用状态类型
pub type SharedState = Arc<Mutex<TiangongState>>;

/// Server 共享上下文：统一持有应用状态、Core 配置提供者与消息路由器
#[derive(Clone)]
pub struct ServerAppContext {
    pub state: SharedState,
    pub config: CoreConfigProvider,
    pub cores: Arc<ServerCoreManager>,
    pub router: Arc<MessageRouter>,
}

impl ServerAppContext {
    pub fn new(
        state: SharedState,
        config: CoreConfigProvider,
        event_bus: Arc<EventBus>,
        memory_handle: Option<tiangong_memory::MemoryHandle>,
    ) -> Self {
        let cores = Arc::new(ServerCoreManager::new(
            state.clone(),
            config.clone(),
            event_bus.clone(),
            memory_handle,
        ));
        let router = Arc::new(MessageRouter::new(state.clone(), event_bus, cores.clone()));
        Self {
            state,
            config,
            cores,
            router,
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
        .append(Route::new("server/shutdown").post(server_ctrl::shutdown))
        .append(ws::ws_route());

    (route, configs)
}
