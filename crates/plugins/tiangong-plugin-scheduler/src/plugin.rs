//! 定时任务进程内插件（issue #156 自注册架构）。
//!
//! [`SchedulerPlugin`] 封装定时任务的工具规格与覆盖处理器。与 browser/terminal 插件
//! 不同，scheduler 不依赖 Tauri 句柄（纯文件存储），但需要宿主注入执行上下文才能
//! 真正触发任务，因此**仅在长期运行的宿主（Desktop / Server）注册**。CLI 这类前台
//! 交互工具不注册本插件——定时任务属于长期运行宿主的能力。
//!
//! 工具规格与覆盖处理器直接在 [`SchedulerPlugin`] 上实现（supertrait 自动收集），
//! 无需在 `register` 中手动注册。
//!
//! 执行上下文（[`SchedulerContext`]）由入口层经 [`SchedulerPlugin::new`] 必填注入：
//! Agent 手动触发 `scheduler_trigger_job` 会经它调用 `execute_job` 真正执行任务（复用
//! 宿主的消息路由）。注入模式与 memory 插件的 `memory_handle` 一致。

use std::sync::Arc;

use tiangong_scheduler::executor::SchedulerContext;

/// 定时任务插件：聚合工具规格与覆盖处理器。
///
/// - `store_base`：可选存储根目录，测试隔离用；`None` 表示用默认 `~/.tiangong/scheduler`。
/// - `context`：调度执行上下文（**必填**）。Agent 手动触发定时任务经它真正执行。
///
/// 不持有 Tauri 句柄或共享 state，可跨 GUI / Server 入口复用。
pub struct SchedulerPlugin {
    pub(crate) store_base: Option<std::path::PathBuf>,
    pub(crate) context: Arc<dyn SchedulerContext>,
}

impl SchedulerPlugin {
    /// 构造插件，必填注入调度执行上下文。
    ///
    /// 入口层（Server / Desktop）在构造插件时传入，让 `scheduler_trigger_job`
    /// 能通过 `execute_job` 真正执行任务。
    pub fn new(context: Arc<dyn SchedulerContext>) -> Self {
        Self {
            store_base: None,
            context,
        }
    }

    /// 指定存储根目录（测试用），返回新的实例。
    pub fn with_store_base(self, store_base: std::path::PathBuf) -> Self {
        Self {
            store_base: Some(store_base),
            ..self
        }
    }
}

impl tiangong_core::core::Plugin for SchedulerPlugin {
    fn id(&self) -> &str {
        "scheduler"
    }

    // 工具规格 / 工具覆盖 / Prompt 段落由 core 通过 supertrait 自动收集
    // （见下方的 ToolSpecProvider / ToolOverrideHandler 实现）。
    // scheduler 无内部状态需要初始化，register 留空。
}

// PromptSectionProvider 使用默认空实现（scheduler 不注入 prompt 段落）
impl tiangong_core::tool_override::PromptSectionProvider for SchedulerPlugin {}
