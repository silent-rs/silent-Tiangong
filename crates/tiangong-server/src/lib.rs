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
use tiangong_connector::manager::ConnectorManager;
use tiangong_core::app_state::TiangongState;
use tiangong_core::permission::TrustMode;
use tokio::sync::Mutex;

use self::api::{ServerAppContext, SharedState, build_routes};
use self::config::load_connectors_config;
use self::remote::event::{EventBus, TiangongEvent};

/// 启动 Server 模式（前台运行，阻塞）
#[allow(deprecated)]
pub fn run_server(host: &str, port: u16, token: Option<String>) -> Result<()> {
    let addr: SocketAddr = format!("{host}:{port}").parse()?;

    tracing::info!("正在初始化应用状态...");
    let state: SharedState = Arc::new(Mutex::new(TiangongState::load_or_default()));
    {
        let mut guard = state.blocking_lock();
        let _ = guard.set_trust_mode(TrustMode::FullTrust);
    }
    let mut app_config = load_tiangong_config();
    app_config.trust_mode = TrustMode::FullTrust;
    let config = app_config.into_core_config_provider();

    // 创建 EventBus
    let event_bus = Arc::new(EventBus::default());
    let app = Arc::new(ServerAppContext::new(state, config, event_bus.clone()));

    // 加载并启动 Connector
    let connector_manager = load_and_start_connectors(event_bus.clone());

    tracing::info!("构建路由...");
    let (api_routes, configs) = build_routes(app, token, event_bus);

    let mut route = Route::new_root().append(api_routes);
    route.set_configs(Some(configs));

    tracing::info!("Server 启动：http://{addr}");
    Server::new().bind(addr).run(route);

    // Server 停止后，停止所有 Connector
    if let Some(manager) = connector_manager {
        // ConnectorManager 在 Server 退出后自动 drop
        tracing::info!("Server 停止，清理 Connector...");
        drop(manager);
    }

    Ok(())
}

/// 加载 Connector 配置并启动已启用的 Connector
fn load_and_start_connectors(event_bus: Arc<EventBus>) -> Option<ConnectorManager> {
    let configs = load_connectors_config();
    if configs.is_empty() {
        tracing::info!("未发现 Connector 配置，跳过 Connector 加载");
        return None;
    }

    let mut manager = ConnectorManager::new();
    let mut enabled_names = Vec::new();

    for config in &configs {
        if !config.enabled {
            tracing::info!(connector = %config.name, "Connector 已禁用，跳过");
            continue;
        }

        match manager.register_from_config(config) {
            Ok(()) => {
                tracing::info!(connector = %config.name, "Connector 已注册");
                enabled_names.push(config.name.clone());
            }
            Err(e) => {
                tracing::error!(connector = %config.name, "注册 Connector 失败: {e}");
                event_bus.publish(TiangongEvent::ConnectorError {
                    name: config.name.clone(),
                    error: format!("注册失败: {e}"),
                });
            }
        }
    }

    // 异步启动所有已启用的 Connector
    let rt = tokio::runtime::Handle::try_current();
    if let Ok(handle) = rt {
        let event_bus_clone = event_bus.clone();
        for name in enabled_names {
            let eb = event_bus_clone.clone();
            // 由于 ConnectorManager 不是 Send，我们在当前线程中 block_on 启动
            // TODO: 后续可改为异步启动
            let name_clone = name.clone();
            match handle.block_on(manager.start(&name)) {
                Ok(()) => {
                    tracing::info!(connector = %name_clone, "Connector 已启动");
                    eb.publish(TiangongEvent::ConnectorStarted(name_clone));
                }
                Err(e) => {
                    tracing::error!(connector = %name_clone, "启动 Connector 失败: {e}");
                    eb.publish(TiangongEvent::ConnectorError {
                        name: name_clone,
                        error: format!("启动失败: {e}"),
                    });
                }
            }
        }
    } else {
        tracing::warn!("未找到 tokio runtime，无法启动 Connector");
    }

    Some(manager)
}

/// 后台运行 Server
pub fn run_daemon(host: &str, port: u16, token: Option<String>) -> Result<()> {
    daemon::run_daemon(host, port, token)
}

/// 停止后台 Server
pub fn stop_daemon() -> Result<()> {
    daemon::stop_daemon()
}
