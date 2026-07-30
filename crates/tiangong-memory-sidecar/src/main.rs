//! Memory System 独立 sidecar 进程。
//!
//! 作为 memory 的唯一 Leader 进程运行，承载全部存储（SQLite/tantivy/lancedb）
//! 原生运行，并通过 TCP IPC 暴露给 Core（CLI/Server/Desktop）访问。
//!
//! 工作流程：
//! 1. 加载 MemoryConfig
//! 2. 竞争 Leader lease（复用 election）
//! 3. 抢到 Leader → 起 MemoryActor + IpcServer + 心跳 → 阻塞运行
//! 4. 已有 Leader → 优雅退出（不重复运行）
//!
//! 见 RFC docs/memory-system/11-memory-sidecar-wasm-bridge.md。

use tiangong_memory::MemoryConfig;
use tiangong_memory::election::{LeaderState, ProcessType, start_or_connect_with_options};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("memory sidecar 启动中...");

    // 加载 memory 配置（模型端点、embedding 等）。
    let options = MemoryConfig::load_or_default().to_options();

    // 竞争 Leader lease。抢到就起 actor + IPC server，没抢到就当 follower。
    let managed = start_or_connect_with_options(options, ProcessType::Sidecar).await?;

    match managed.state() {
        LeaderState::Leader => {
            tracing::info!("memory sidecar 已成为 Leader，开始服务");
            // 阻塞等待终止信号（Ctrl+C / SIGTERM）。
            // ManagedMemory 持有 actor + IPC bridge + 心跳，Drop 时自动清理。
            tokio::signal::ctrl_c()
                .await
                .map_err(|e| anyhow::anyhow!("等待终止信号失败: {e}"))?;
            tracing::info!("收到终止信号，memory sidecar 退出");
        }
        LeaderState::Follower { pid } => {
            tracing::info!("已有 memory Leader 运行中（pid={pid}），本 sidecar 无需重复启动，退出");
        }
    }

    // managed Drop：停心跳、删 endpoint 文件、释放 leader.lock
    drop(managed);
    Ok(())
}
