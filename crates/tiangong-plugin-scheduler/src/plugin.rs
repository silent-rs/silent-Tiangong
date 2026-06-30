//! 定时任务进程内插件（issue #156 自注册架构）。
//!
//! [`SchedulerPlugin`] 封装定时任务的工具规格与覆盖处理器，在 engine 创建/重建时
//! 自行注册。与 browser/terminal 插件不同，scheduler 不依赖 Tauri 句柄（纯文件存储），
//! 因此可在 GUI / CLI / Server 全入口无条件启用。

use std::sync::Arc;

use tiangong_core::core::Plugin;
use tiangong_core::runtime::RuntimeEngine;

use crate::handler::SchedulerToolOverride;

/// 定时任务插件：向 engine 注入工具规格与覆盖处理器。
///
/// 无外部依赖（不持有 Tauri 句柄或共享 state），可跨 GUI / CLI / Server 入口复用。
#[derive(Clone, Default)]
pub struct SchedulerPlugin {
    handler: Arc<SchedulerToolOverride>,
}

impl SchedulerPlugin {
    pub fn new() -> Self {
        Self {
            handler: Arc::new(SchedulerToolOverride::new()),
        }
    }
}

impl Plugin for SchedulerPlugin {
    fn id(&self) -> &str {
        "scheduler"
    }

    fn register(&self, engine: &RuntimeEngine) {
        // 1) 注入工具规格：6 个独立工具交给 LLM
        engine.register_tool_spec_provider(self.handler.clone());

        // 2) 注册覆盖处理器：所有 scheduler_* 工具名都路由到同一 handler，
        //    handler 内部按 call.name 分发。优先级高于 LocalToolExecutor 默认逻辑。
        for tool_name in [
            crate::handler::TOOL_CREATE_JOB,
            crate::handler::TOOL_LIST_JOBS,
            crate::handler::TOOL_UPDATE_JOB,
            crate::handler::TOOL_DELETE_JOB,
            crate::handler::TOOL_TRIGGER_JOB,
            crate::handler::TOOL_GET_JOB_RUNS,
        ] {
            engine.register_tool_override(tool_name, self.handler.clone());
        }
    }
}
