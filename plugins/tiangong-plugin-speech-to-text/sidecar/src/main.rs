//! Speech-To-Text 独立 sidecar 进程。

mod record_session;
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
        business_protocol = tiangong_plugin_speech_to_text_protocol::STT_PROTOCOL_VERSION,
        "speech-to-text sidecar 启动中..."
    );

    let config = tiangong_plugin_sidecar::SidecarConfig::new("speech-to-text");
    tiangong_plugin_sidecar::run(config, || Ok(std::sync::Arc::new(service::SttService))).await
}
