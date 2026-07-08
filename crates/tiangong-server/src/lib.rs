pub mod api;
pub mod auth;
pub mod config;
pub mod daemon;
pub mod remote;
pub mod scheduler;
pub mod webhook;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use silent::Scheduler;
use silent::prelude::*;
use tiangong_app_state::app_state::TiangongState;
use tiangong_config::load_tiangong_config;
use tiangong_core::permission::TrustMode;
use tiangong_scheduler::executor::SchedulerContext;
use tokio::sync::Mutex;

use self::api::{ServerAppContext, SharedState, build_routes};
use self::remote::event::EventBus;
use self::scheduler::context::ServerSchedulerContext;

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
    let (api_routes, configs) = build_routes(app.clone(), token, event_bus);

    let mut route = Route::new_root().append(api_routes);
    route.set_configs(Some(configs));

    // 初始化调度器，恢复已启用的 cron job
    let scheduler_ctx: Arc<dyn SchedulerContext> = Arc::new(ServerSchedulerContext {
        state: app.state.clone(),
        cores: app.cores.clone(),
    });
    tokio::spawn(async move {
        tiangong_scheduler::executor::restore_cron_jobs(scheduler_ctx).await;
        Scheduler::schedule(silent::SCHEDULER.clone()).await;
    });

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

// ── 嵌入式 Server（Desktop App 内部运行）──────────────────────────

/// 嵌入式 Server 的关闭句柄
pub struct EmbeddedServerHandle {
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EmbeddedServerHandle {
    /// 停止嵌入式 Server
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // 等待 server 线程退出
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 启动嵌入式 Server（非阻塞，在独立线程中运行）
///
/// 创建独立的 tokio runtime 运行 server，通过 `Arc<Mutex<TiangongState>>`
/// 与 Desktop app 共享状态。不依赖调用方是否处于 tokio 上下文。
#[allow(deprecated)]
pub fn run_embedded(
    host: &str,
    port: u16,
    token: Option<String>,
    state: SharedState,
    config: tiangong_core::core_config::CoreConfigProvider,
) -> Result<EmbeddedServerHandle> {
    let addr: SocketAddr = format!("{host}:{port}").parse()?;

    let event_bus = Arc::new(EventBus::default());
    let app = Arc::new(ServerAppContext::new(state, config, event_bus.clone()));

    tracing::info!("构建嵌入式 Server 路由...");
    let (api_routes, configs) = build_routes(app.clone(), token, event_bus);

    let mut route = Route::new_root().append(api_routes);
    route.set_configs(Some(configs));

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tracing::info!("嵌入式 Server 启动：http://{addr}");
    let thread = std::thread::Builder::new()
        .name("tiangong-embedded-server".to_string())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build embedded server runtime");
            rt.block_on(async move {
                // 初始化调度器
                let scheduler_ctx: Arc<dyn SchedulerContext> = Arc::new(ServerSchedulerContext {
                    state: app.state.clone(),
                    cores: app.cores.clone(),
                });
                tokio::spawn(async move {
                    tiangong_scheduler::executor::restore_cron_jobs(scheduler_ctx).await;
                    Scheduler::schedule(silent::SCHEDULER.clone()).await;
                });

                let server = Server::new()
                    .bind(addr)
                    .with_shutdown(std::time::Duration::from_secs(5));
                tokio::select! {
                    _ = server.serve(route) => {}
                    _ = shutdown_rx => {
                        tracing::info!("嵌入式 Server 收到关闭信号");
                    }
                }
            });
        })?;

    Ok(EmbeddedServerHandle {
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    })
}
