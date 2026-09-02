//! Command 独立 sidecar 进程。
//!
//! 作为 Command 的唯一常驻进程运行，承载全部 tokio 子进程 spawn（run_command /
//! run_shell）与受控 env 注入。访问边界由宿主沙箱实施，通过 TCP IPC 暴露给
//! 运行时访问。
//!
//! 工作流程：
//! 1. 淘汰制单例判定（endpoint 文件 + TCP 端口可达性）
//! 2. 端口可达 → 已有实例，优雅退出
//! 3. 端口不可达 → 起 IPC server + CommandService → 阻塞运行
//!
//! 单例与 IPC 由 `tiangong-plugin-sidecar` 通用运行库提供。无 GUI 句柄、无管理
//! API 直调、无跨实例共享状态，下沉最干净。

mod exec;
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
        business_protocol = tiangong_plugin_command_protocol::COMMAND_PROTOCOL_VERSION,
        "command sidecar 启动中..."
    );

    let config = tiangong_plugin_sidecar::SidecarConfig::new("command");
    tiangong_plugin_sidecar::run(config, || {
        Ok(std::sync::Arc::new(service::CommandService::new()?))
    })
    .await
}
