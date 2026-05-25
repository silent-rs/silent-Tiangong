pub mod api;
pub mod auth;
pub mod config;
pub mod daemon;
pub mod remote;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use silent::Scheduler;
use silent::prelude::*;
use tiangong_config::load_tiangong_config;
use tiangong_core::app_state::TiangongState;
use tiangong_core::permission::TrustMode;
use tiangong_core::scheduler::model::TriggerType;
use tiangong_core::scheduler::store::JobStore;
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

    // 初始化调度器，恢复已启用的 cron job
    restore_cron_jobs();
    tokio::spawn(async {
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

/// 从 job store 加载已启用的 cron job 并注册到 silent scheduler
fn restore_cron_jobs() {
    let store = match JobStore::open() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("恢复定时任务失败，跳过：{e}");
            return;
        }
    };

    let jobs = match store.list_enabled_jobs_by_type(&TriggerType::Cron) {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!("查询定时任务失败，跳过：{e}");
            return;
        }
    };

    // blocking_lock 因为还在同步上下文中
    let rt = tokio::runtime::Handle::current();
    rt.block_on(async {
        let mut scheduler = silent::SCHEDULER.lock().await;
        for job in jobs {
            let schedule = match &job.schedule {
                Some(s) => s.clone(),
                None => continue,
            };
            let job_id = job.id.clone();
            let process_time = match schedule.try_into() {
                Ok(pt) => pt,
                Err(e) => {
                    tracing::warn!("解析 cron 表达式失败 [{}]: {e}", job.schedule.unwrap());
                    continue;
                }
            };
            let task = silent::Task::create_with_action_async(
                job.id.clone(),
                process_time,
                job.name.clone(),
                Arc::new(move || {
                    let job_id = job_id.clone();
                    Box::pin(async move {
                        tracing::info!("定时任务触发：{job_id}");
                        // TODO: 查找或创建 session → RuntimeEngine 执行 → 记录 JobRun
                        Ok(())
                    })
                }),
            );
            if let Err(e) = scheduler.add_task(task) {
                tracing::warn!("注册定时任务失败 [{}]：{e}", job.id);
            } else {
                tracing::info!("已恢复定时任务：{} [{}]", job.name, job.id);
            }
        }
    });
}
