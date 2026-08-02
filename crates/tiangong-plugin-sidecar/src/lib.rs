//! 插件 sidecar 通用运行库。
//!
//! 业务中立的 sidecar 进程通用能力，与宿主侧 `tiangong-plugin-runtime`
//! （加载 WASM / 启动连接 sidecar / 转发请求）边界清晰。
//!
//! # 包含
//! - [`singleton`]：跨进程单例选举（Leader 互斥 + 心跳 + follower 退出）
//! - [`identity`]：sidecar 配置与实例身份
//! - [`endpoint`]：运行时目录解析、endpoint 文件发布/读取/清理
//! - [`server`]：通用 IPC server（TCP loopback + 动态端口 + token 鉴权 + JSON Lines 帧）
//! - [`shutdown`]：跨平台退出信号（Ctrl+C / SIGTERM）
//!
//! # 不包含
//! 任何插件编号、业务操作名、tantivy/模型等业务依赖、App/CLI/Server 类型、
//! WASM guest 类型、某插件专属的数据目录规则。
//!
//! # 快速启动
//! ```no_run
//! # use std::sync::Arc;
//! # use async_trait::async_trait;
//! # use tiangong_plugin_sidecar::{SidecarConfig, SidecarService, run};
//! # use tiangong_plugin_runtime::protocol::{Request, Response};
//! # struct MyService;
//! # #[async_trait]
//! # impl SidecarService for MyService {
//! #     async fn dispatch(&self, _: Request) -> Response { unimplemented!() }
//! # }
//! # async fn demo() -> anyhow::Result<()> {
//! let config = SidecarConfig::new("my-plugin");
//! run(config, || Ok(Arc::new(MyService))).await
//! # }
//! ```

pub mod endpoint;
pub mod identity;
pub mod server;
pub mod shutdown;
pub mod singleton;

pub use identity::{SidecarConfig, SidecarIdentity};
pub use server::IpcBridge;
pub use singleton::{LeaderInfo, LeaderState, ManagedSidecar, SidecarService, start_or_connect};

use std::sync::Arc;

use anyhow::Result;

/// 一键启动 sidecar。
///
/// 完整流程：选举 → Leader 起 IPC server + 心跳，阻塞等终止信号；Follower 退出。
/// `service_factory` 仅在确认成为 Leader 后调用（被淘汰的 follower 候选不会构造
/// service，避免白白打开数据、占用资源）。
///
/// 各插件 main.rs 只需构造好 `SidecarConfig` + 业务 service 工厂，调本函数即可。
/// 若需在启动前做业务前置（如加载配置、恢复数据目录），在工厂闭包内完成。
pub async fn run<F>(config: SidecarConfig, service_factory: F) -> Result<()>
where
    F: FnOnce() -> anyhow::Result<Arc<dyn SidecarService>>,
{
    let managed = start_or_connect(&config, service_factory)?;
    match managed.state() {
        LeaderState::Leader => {
            tracing::info!("{} sidecar 已成为 Leader，开始服务", config.service);
            shutdown::wait_for_shutdown_signal().await?;
            tracing::info!("收到终止信号，{} sidecar 退出", config.service);
        }
        LeaderState::Follower { pid } => {
            tracing::info!(
                "已有 {} Leader 运行中（pid={pid}），本 sidecar 无需重复启动，退出",
                config.service
            );
        }
    }
    drop(managed);
    Ok(())
}
