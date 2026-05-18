pub mod api;
pub mod auth;
pub mod config;
pub mod daemon;
pub mod remote;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use silent::prelude::*;
use tiangong_config::load_tiangong_config;
use tiangong_core::app_state::TiangongState;
use tiangong_core::permission::TrustMode;
use tokio::sync::Mutex;

use self::api::{ServerAppContext, SharedState, build_routes};
use self::remote::event::EventBus;

/// 启动 Server 模式（前台运行，阻塞）
#[allow(deprecated)]
pub fn run_server(host: &str, port: u16, token: Option<String>) -> Result<()> {
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _runtime_guard = runtime.enter();

    let mut app_config = load_tiangong_config();
    app_config.trust_mode = TrustMode::FullTrust;
    let core_config = app_config.to_core_config();

    let config = tiangong_core::core_config::CoreConfigProvider::new(core_config);

    tracing::info!("正在初始化应用状态...");
    let state: SharedState = Arc::new(Mutex::new(TiangongState::load_or_default()));
    {
        let mut guard = state.blocking_lock();
        let _ = guard.set_trust_mode(TrustMode::FullTrust);
    }

    let event_bus = Arc::new(EventBus::default());
    let app = Arc::new(ServerAppContext::new(state, config, event_bus.clone()));

    tracing::info!("构建路由...");
    let (api_routes, configs) = build_routes(app, token, event_bus);

    let mut route = Route::new_root().append(api_routes);
    route.set_configs(Some(configs));

    tracing::info!("Server 启动：http://{addr}");
    Server::new().bind(addr).run(route);

    Ok(())
}

/// 后台运行 Server
pub fn run_daemon(host: &str, port: u16, token: Option<String>) -> Result<()> {
    daemon::run_daemon(host, port, token)
}

/// 停止后台 Server
pub fn stop_daemon() -> Result<()> {
    daemon::stop_daemon()
}
