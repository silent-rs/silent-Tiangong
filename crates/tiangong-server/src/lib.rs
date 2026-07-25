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
use tiangong_core::permission::TrustMode;
use tiangong_scheduler::executor::SchedulerContext;
use tokio::sync::Mutex;

use self::api::{ServerAppContext, SharedState, build_routes};
use self::remote::backend::ServerCoreBackend;
use self::remote::event::EventBus;

/// 启动 Server 模式（前台运行，阻塞）
///
/// 获取 Bot 管理所有权锁（issue #286）：若 Desktop 已持 `desktop.lock` 则拒绝启动，
/// 避免 Desktop 与独立 Server 同时管理 bot。锁句柄随函数返回释放（正常退出），
/// 崩溃/强杀由 OS 自动释放。阶段 2b 将补信号处理确保收到 SIGTERM 时优雅停 bot。
#[allow(deprecated)]
pub fn run_server(host: &str, port: u16, token: Option<String>) -> Result<()> {
    use tiangong_config::lock::{OwnerKind, OwnershipLock};
    match OwnershipLock::acquire(OwnerKind::Server)? {
        Ok(lock) => {
            tracing::info!("已获取 Server Bot 管理所有权锁");
            // 持有锁至函数返回；存入变量防止过早 drop。
            let _bot_ownership = lock;
            run_server_inner(host, port, token)
        }
        Err(owner) => Err(anyhow::anyhow!(
            "Bot 管理权已被 {} 占用，无法启动独立 Server。请先退出 {} 后重试。",
            owner_label(owner),
            owner_label(owner)
        )),
    }
}

fn owner_label(owner: tiangong_config::lock::OwnerKind) -> &'static str {
    use tiangong_config::lock::OwnerKind;
    match owner {
        OwnerKind::Desktop => "Desktop",
        OwnerKind::Server => "另一个独立 Server",
    }
}

fn run_server_inner(host: &str, port: u16, token: Option<String>) -> Result<()> {
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _runtime_guard = runtime.enter();

    tracing::info!("正在初始化应用状态...");
    let mut app_state = tiangong_app_state::app_state::TiangongState::new();
    let storage_root = app_state.config.storage_root.clone();
    app_state.config.default_trust_mode = TrustMode::FullTrust;
    let core_manager = app_state.core_manager.clone();
    core_manager
        .config()
        .replace(app_state.config.to_core_config());
    app_state.workspace_dir = std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    app_state.agent_config.trust_mode = TrustMode::FullTrust;
    let state: SharedState = Arc::new(Mutex::new(app_state));

    let event_bus = Arc::new(EventBus::default());
    let app = Arc::new(ServerAppContext::new(
        state,
        core_manager,
        event_bus.clone(),
        storage_root,
    ));

    tracing::info!("构建路由...");
    let (api_routes, configs) = build_routes(app.clone(), token, event_bus);

    let mut route = Route::new_root().append(api_routes);
    route.set_configs(Some(configs));

    // 初始化调度器，恢复已启用的 cron job
    let scheduler_ctx = app.scheduler_context.clone();
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
/// 创建独立的 tokio runtime 运行 HTTP，但 Core、调度上下文和 MCP 管理实例均由
/// Desktop 注入。HTTP runtime 不拥有会话写入器，也不会恢复或启动全局调度器。
pub struct EmbeddedServerDependencies {
    pub state: SharedState,
    pub config: tiangong_core::core_config::CoreConfigProvider,
    pub core_backend: Arc<dyn ServerCoreBackend>,
    pub scheduler_context: Arc<dyn SchedulerContext>,
    pub mcp_plugin: Arc<tiangong_plugin_mcp::McpPlugin>,
    /// Bot 管理句柄（issue #286）：嵌入式 Server 复用 Desktop 的 bot_store/bot_runtime，
    /// 使 HTTP /api/v1/bots 在 Desktop 下也指向同一管理实例。
    pub bot_store: Arc<tiangong_bots::BotStore>,
    pub bot_runtime: Arc<tiangong_bots::BotRuntime>,
    pub event_bus: Arc<EventBus>,
}

#[allow(deprecated)]
pub fn run_embedded(
    host: &str,
    port: u16,
    token: Option<String>,
    dependencies: EmbeddedServerDependencies,
) -> Result<EmbeddedServerHandle> {
    let addr: SocketAddr = format!("{host}:{port}").parse()?;

    let EmbeddedServerDependencies {
        state,
        config,
        core_backend,
        scheduler_context,
        mcp_plugin,
        bot_store,
        bot_runtime,
        event_bus,
    } = dependencies;
    let app = Arc::new(ServerAppContext::with_backend(
        state,
        config,
        event_bus.clone(),
        core_backend,
        scheduler_context,
        mcp_plugin,
        bot_store,
        bot_runtime,
    ));

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
                // Desktop 已经恢复并启动全局调度器；内嵌 HTTP 只复用注入的执行上下文。
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
