mod service;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let config = tiangong_plugin_sidecar::SidecarConfig::new("coding");
    tiangong_plugin_sidecar::run(config, || {
        Ok(std::sync::Arc::new(service::CodingService::new()?))
    })
    .await
}
