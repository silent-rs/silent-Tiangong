//! MCP 独立 sidecar 进程。
//!
//! 作为 MCP 的唯一常驻进程运行，承载全部 rmcp 客户端（stdio/HTTP transport）、
//! capability 后台探测与缓存，并通过 TCP IPC 暴露给运行时访问。
//!
//! 工作流程：
//! 1. 竞争 Leader lease（leader.lock 原子文件互斥）
//! 2. 抢到 Leader → 起 IPC server + capability 调度 → 阻塞运行
//! 3. 已有 Leader → 优雅退出（不重复运行）
//!
//! 选举与 IPC 协议对齐 Memory sidecar，运行时按 `plugin.json` 启动本进程。

mod capability;
mod client;
mod election;
mod execution;
mod ipc;
mod paths;
mod service;
mod validate;

use crate::election::{LeaderState, start_or_connect};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!(
        business_protocol = tiangong_plugin_mcp_protocol::MCP_PROTOCOL_VERSION,
        "mcp sidecar 启动中..."
    );

    // 竞争 Leader lease。抢到就起 IPC server + capability，没抢到就退出。
    let managed = start_or_connect().await?;

    match managed.state() {
        LeaderState::Leader => {
            tracing::info!("mcp sidecar 已成为 Leader，开始服务");
            wait_for_shutdown_signal().await?;
            tracing::info!("收到终止信号，mcp sidecar 退出");
        }
        LeaderState::Follower { pid } => {
            tracing::info!("已有 mcp Leader 运行中（pid={pid}），本 sidecar 无需重复启动，退出");
        }
    }

    drop(managed);
    Ok(())
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() -> anyhow::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result?,
        _ = terminate.recv() => {},
    }
    Ok(())
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() -> anyhow::Result<()> {
    tokio::signal::ctrl_c().await?;
    Ok(())
}
