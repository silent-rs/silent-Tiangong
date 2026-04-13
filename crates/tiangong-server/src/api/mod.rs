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
use tiangong_core::app_state::TiangongState;
use tiangong_gateway::event::EventBus;
use tokio::sync::Mutex;

/// 共享应用状态类型
pub type SharedState = Arc<Mutex<TiangongState>>;

/// 认证 Token 包装（用于注入到 Configs 中）
#[derive(Clone, Debug)]
pub struct AuthToken(pub Option<String>);

/// 构建完整的 API 路由树，通过 Configs 注入共享状态和 Token
#[allow(deprecated)]
pub fn build_routes(
    state: SharedState,
    token: Option<String>,
    event_bus: Arc<EventBus>,
) -> (Route, Configs) {
    let mut configs = Configs::default();
    configs.insert(state);
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
                        .delete(sessions::delete_session),
                ),
        )
        .append(Route::new("mcp").get(mcp::list_mcp))
        .append(Route::new("skills").get(skills::list_skills))
        .append(Route::new("server/shutdown").post(server_ctrl::shutdown))
        .append(ws::ws_route());

    (route, configs)
}
