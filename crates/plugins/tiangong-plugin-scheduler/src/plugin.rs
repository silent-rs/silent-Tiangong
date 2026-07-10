//! 定时任务进程内插件（issue #156 自注册架构）。
//!
//! [`SchedulerPlugin`] 封装定时任务的工具规格与覆盖处理器。与 browser/terminal 插件
//! 不同，scheduler 不依赖 Tauri 句柄（纯文件存储），因此可在 GUI / CLI / Server
//! 全入口无条件启用。
//!
//! 工具规格与覆盖处理器直接在 [`SchedulerPlugin`] 上实现（supertrait 自动收集），
//! 无需在 `register` 中手动注册。

/// 定时任务插件：聚合工具规格与覆盖处理器。
///
/// 无外部依赖（不持有 Tauri 句柄或共享 state），可跨 GUI / CLI / Server 入口复用。
#[derive(Clone, Default)]
pub struct SchedulerPlugin {
    /// 可选的存储根目录，用于测试隔离。生产环境为 None，使用默认的 `~/.tiangong/scheduler`。
    pub(crate) store_base: Option<std::path::PathBuf>,
}

impl SchedulerPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定存储根目录（测试用），返回新的实例。
    #[cfg(test)]
    pub(crate) fn with_store_base(store_base: std::path::PathBuf) -> Self {
        Self {
            store_base: Some(store_base),
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
