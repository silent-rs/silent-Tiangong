//! 定时任务进程内插件（issue #156 自注册架构）。
//!
//! 把定时任务（Cron Job）的工具能力以插件形式注入 Agent，替代旧的
//! `LocalToolExecutor` 位置参数实现。核心收益：工具调用直接从命名参数 JSON
//! 按 key 取参，彻底消除参数顺序错位导致的「任务不存在」问题。
//!
//! 与 browser/terminal 插件不同，scheduler 不依赖 Tauri 句柄，纯文件存储，但需要
//! 宿主注入 [`SchedulerContext`] 才能真正触发任务。因此**仅在长期运行的宿主
//!（Desktop / Server）注册**；CLI 这类前台交互工具不注册本插件——定时任务属于
//! 长期运行宿主的能力。
//!
//! [`SchedulerContext`]: tiangong_scheduler::executor::SchedulerContext

pub mod handler;
pub mod plugin;

pub use plugin::SchedulerPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;
use tiangong_scheduler::executor::SchedulerContext;

/// 构造定时任务插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
///
/// 必填注入 `context`：让 Agent 手动触发 `scheduler_trigger_job` 时能通过 `execute_job`
/// 真正执行任务。Server / Desktop 入口在构建 Core 时调用本函数。
pub fn build_plugin(context: Arc<dyn SchedulerContext>) -> Arc<dyn Plugin> {
    Arc::new(SchedulerPlugin::new(context))
}

/// 构造定时任务插件列表，供 Server / Desktop 注入 core 时使用。
pub fn default_plugins(context: Arc<dyn SchedulerContext>) -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin(context)]
}
