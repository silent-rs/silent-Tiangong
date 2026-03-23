pub mod api;
pub mod auth;
pub mod config;
pub mod daemon;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use silent::prelude::*;
use tiangong_core::app_state::TiangongState;
use tokio::sync::Mutex;

use self::api::{SharedState, build_routes};

/// 启动 Server 模式（前台运行，阻塞）
#[allow(deprecated)]
pub fn run_server(host: &str, port: u16, token: Option<String>) -> Result<()> {
    let addr: SocketAddr = format!("{host}:{port}").parse()?;

    tracing::info!("正在初始化应用状态...");
    let state: SharedState = Arc::new(Mutex::new(TiangongState::load_or_default()));

    tracing::info!("构建路由...");
    let (api_routes, configs) = build_routes(state, token);

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
