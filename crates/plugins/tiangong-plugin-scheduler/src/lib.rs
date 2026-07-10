//! 定时任务进程内插件（issue #156 自注册架构）。
//!
//! 把定时任务（Cron Job）的工具能力以插件形式注入 Agent，替代旧的
//! `LocalToolExecutor` 位置参数实现。核心收益：工具调用直接从命名参数 JSON
//! 按 key 取参，彻底消除参数顺序错位导致的「任务不存在」问题。
//!
//! 与 browser/terminal 插件不同，scheduler 不依赖 Tauri 句柄，纯文件存储，
//! 因此可在 GUI / CLI / Server 全入口无条件启用。

pub mod handler;
pub mod plugin;

pub use plugin::SchedulerPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造定时任务插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
///
/// 与 browser/terminal 的 `build_plugin` 对齐。scheduler 不依赖 Tauri 句柄，
/// 始终返回 `Some`。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(SchedulerPlugin::new())
}

/// 构造默认的定时任务插件列表，供各入口（CLI / Server）注入 core 时使用。
///
/// GUI 入口已在 `main.rs` 显式注册；CLI / Server 入口调用此函数获取插件并传入
/// `TiangongCore::builder().plugins(...)`。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
