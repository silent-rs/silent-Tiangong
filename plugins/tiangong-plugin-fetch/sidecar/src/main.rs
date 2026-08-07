//! Fetch 独立 sidecar 进程。
//!
//! 作为 Fetch 的唯一常驻进程运行，承载全部网络抓取（reqwest 阻塞客户端）、SSRF
//! 防护与 download 落盘，通过 TCP IPC 暴露给运行时访问。
//!
//! 工作流程：
//! 1. 淘汰制单例判定（endpoint 文件 + TCP 端口可达性）
//! 2. 端口可达 → 已有实例，优雅退出（不重复运行）
//! 3. 端口不可达 → 起 IPC server + FetchService → 阻塞运行
//!
//! 单例与 IPC 由 `tiangong-plugin-sidecar` 通用运行库提供，运行时按 `plugin.json`
//! 启动本进程。

mod fetch;
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
        business_protocol = tiangong_plugin_fetch_protocol::FETCH_PROTOCOL_VERSION,
        "fetch sidecar 启动中..."
    );

    let config = tiangong_plugin_sidecar::SidecarConfig::new("fetch");
    tiangong_plugin_sidecar::run(config, || {
        Ok(std::sync::Arc::new(service::FetchService::new()?))
    })
    .await
}
