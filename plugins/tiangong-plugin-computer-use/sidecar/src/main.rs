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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

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
