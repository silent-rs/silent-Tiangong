//! Text-To-Speech 独立 sidecar 进程。
//!
//! 读取天工 models.json，按 TTS 能力解析端点，调用供应商接口合成语音，
//! 音频落盘到 `~/.tiangong/media/`，返回本地文件路径。

mod service;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!(
        business_protocol = tiangong_plugin_text_to_speech_protocol::TTS_PROTOCOL_VERSION,
        "text-to-speech sidecar 启动中..."
    );

    let config = tiangong_plugin_sidecar::SidecarConfig::new("text-to-speech");
    tiangong_plugin_sidecar::run(config, || Ok(std::sync::Arc::new(service::TtsService))).await
}
