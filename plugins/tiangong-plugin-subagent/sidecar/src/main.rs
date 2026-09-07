//! Subagent 插件 sidecar：持久 Agent 统一交互总线。
//!
//! 完全插件化的 Subagent 核心——宿主零 Subagent 代码：
//! - AI 工具与管理页操作经 `sidecar.<operation>` 请求到达（工具直连）；
//! - 状态变化经 `emit_notification("subagent.event", ...)` 推送，管理页订阅刷新；
//! - 重要反馈（完成/阻塞/审批/失败）先落盘再经本机 server 投回激活会话；
//! - managed 运行实例随宿主退出清理（宿主先调 `subagent_shutdown` 优雅关闭，
//!   信号与进程组级联兜底）。

mod agent_store;
mod delivery;
mod mcp;
mod memory;
mod paths;
mod runner;
mod runtime_store;
mod service;
mod sessions;

use std::sync::Arc;

use tiangong_plugin_sidecar::SidecarConfig;

/// 预读首条非空帧（阻塞至对端发出首帧）：用于协议探测；EOF 返回 None
///（无对端时交还原生入口的常规行为）。
fn peek_first_frame() -> anyhow::Result<Option<String>> {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let mut guard = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = guard.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            return Ok(Some(line));
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    tracing::info!("subagent sidecar 启动中...");

    // 多协议自动探测：同一入口同时服务宿主（stdio IPC 帧）与外部 agent
    // 工具（标准 MCP JSON-RPC）——预读首帧判定协议，宿主帧回喂原生循环。
    let first_line = peek_first_frame()?;
    let is_mcp = first_line
        .as_deref()
        .and_then(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .is_some_and(|frame| frame.get("jsonrpc").is_some() && frame.get("method").is_some());
    if is_mcp {
        let service = Arc::new(service::SubagentService::new()?);
        let workspace = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| std::env::var("HOME").unwrap_or_else(|_| ".".to_string()));
        return mcp::run_mcp(service, workspace, first_line).await;
    }

    if tiangong_plugin_sidecar::stdio::stdio_requested() {
        return tiangong_plugin_sidecar::stdio::run_stdio_with_first_line(
            || {
                let service = Arc::new(service::SubagentService::new()?);
                spawn_signal_cleanup(Arc::clone(&service));
                Ok(service)
            },
            first_line,
        )
        .await;
    }
    let config = SidecarConfig::new("subagent");
    tiangong_plugin_sidecar::run(config, || {
        let service = Arc::new(service::SubagentService::new()?);
        spawn_signal_cleanup(Arc::clone(&service));
        Ok(service)
    })
    .await
}

/// 终止信号兜底：宿主 SIGKILL 前的 SIGTERM / 手动 kill 时同步清理 managed 运行。
fn spawn_signal_cleanup(service: Arc<service::SubagentService>) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("注册 SIGTERM 监听失败");
            let mut sigint =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                    .expect("注册 SIGINT 监听失败");
            tokio::select! {
                _ = sigterm.recv() => {}
                _ = sigint.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.ok();
        }
        tracing::info!("收到终止信号，清理 managed 运行实例");
        service.begin_shutdown().await;
    });
}
