//! Screenshot Input 独立 sidecar 进程。

mod capture;
mod platform;
mod service;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!(
        business_protocol = tiangong_plugin_screenshot_input_protocol::SCREENSHOT_PROTOCOL_VERSION,
        "screenshot-input sidecar 启动中..."
    );

    let config = tiangong_plugin_sidecar::SidecarConfig::new("screenshot-input");
    tiangong_plugin_sidecar::run(config, || {
        Ok(std::sync::Arc::new(service::ScreenshotService))
    })
    .await
}
