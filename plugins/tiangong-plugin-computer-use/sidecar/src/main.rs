//! Computer Use 独立 sidecar 进程。
//!
//! 作为 Computer Use 的唯一常驻进程运行，承载当前系统的原生无障碍接口访问：
//! - Windows：UI Automation
//! - macOS：AXUIElement / AXObserver
//! - Linux：AT-SPI2
//!
//! 通过 TCP IPC 暴露给运行时访问。单例与 IPC 由 `tiangong-plugin-sidecar` 通用运行库提供。
//! sidecar 必须区分“平台不支持”“没有图形会话”“尚未授权”和“目标应用未提供控件树”，
//! 并返回明确结果，不因此导致宿主退出。

use tiangong_plugin_computer_use_sidecar::ComputerUseService;

/// 服务主循环（原 main 主体）：初始化 sidecar IPC 并阻塞运行至退出。
async fn run_service() -> anyhow::Result<()> {
    tracing::info!(
        business_protocol = tiangong_plugin_computer_use_protocol::COMPUTER_USE_PROTOCOL_VERSION,
        "computer-use sidecar 启动中..."
    );
    let config = tiangong_plugin_sidecar::SidecarConfig::new("computer-use");
    tiangong_plugin_sidecar::run(config, || {
        Ok(std::sync::Arc::new(ComputerUseService::new()?))
    })
    .await
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    // macOS：AppKit 断言 NSApplication/NSWindow 只能在进程主线程使用，
    // 虚拟指针 overlay（RFC 0018）因此占用主线程，服务循环交给工作
    // 线程；服务退出后经 request_shutdown 归还主线程并透传结果。
    #[cfg(target_os = "macos")]
    {
        let result_slot = std::sync::Arc::new(std::sync::Mutex::new(None::<anyhow::Result<()>>));
        let slot = result_slot.clone();
        std::thread::Builder::new()
            .name("sidecar-service".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime 构建失败");
                let outcome = runtime.block_on(run_service());
                *slot.lock().unwrap() = Some(outcome);
                tiangong_plugin_computer_use_sidecar::backend::overlay::request_shutdown();
            })
            .expect("sidecar 服务线程启动失败");
        tiangong_plugin_computer_use_sidecar::backend::overlay::run_main_loop();
        return result_slot.lock().unwrap().take().unwrap_or_else(|| Ok(()));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(run_service())
    }
}
