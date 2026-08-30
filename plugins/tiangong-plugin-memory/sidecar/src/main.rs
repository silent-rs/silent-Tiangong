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
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!(
        business_protocol = tiangong_plugin_memory_protocol::MEMORY_PROTOCOL_VERSION,
        "memory sidecar 启动中..."
    );

    if let Some(plugin_data_dir) =
        std::env::var_os("TIANGONG_PLUGIN_DATA_DIR").filter(|value| !value.is_empty())
    {
        tiangong_memory::recover_plugin_data_dir(std::path::Path::new(&plugin_data_dir))?;
    }

    // 加载 memory 配置（模型端点、embedding 等）。
    let options = MemoryConfig::load_or_default().to_options();

    // 竞争 Leader lease。抢到就起 actor + IPC server，没抢到就当 follower。
    let managed = start_or_connect_with_options(options, ProcessType::Sidecar).await?;

    match managed.state() {
        LeaderState::Leader => {
            tracing::info!("memory sidecar 已成为 Leader，开始服务");
            if tiangong_plugin_sidecar::stdio::stdio_requested() {
                // stdio 传输：宿主一对一管理生命周期，进通用应答循环；
                // actor/心跳由 managed 持有，stdin 关闭随 Drop 清理。
                // 进度通道（IPC 连接形态）不可用，其余分发语义一致。
                let handle = managed.handle();
                return tiangong_plugin_sidecar::stdio::run_stdio(move || {
                    Ok(std::sync::Arc::new(MemoryStdioService {
                        handle,
                        _managed: managed,
                    }))
                })
                .await;
            }
            // 阻塞等待终止信号（Ctrl+C / SIGTERM）。
            // ManagedMemory 持有 actor + IPC bridge + 心跳，Drop 时自动清理。
            wait_for_shutdown_signal().await?;
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

/// stdio 传输适配：把通用帧协议接到 memory 的插件请求分发。
struct MemoryStdioService {
    handle: tiangong_memory::MemoryHandle,
    /// 持有 Leader 的 actor、IPC bridge 与心跳，随服务存活。
    _managed: tiangong_memory::election::ManagedMemory,
}

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for MemoryStdioService {
    async fn dispatch(
        &self,
        request: tiangong_plugin_runtime::protocol::Request,
    ) -> tiangong_plugin_runtime::protocol::Response {
        tiangong_memory::ipc::dispatch_checked_plugin_request(self.handle.clone(), request).await
    }
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
