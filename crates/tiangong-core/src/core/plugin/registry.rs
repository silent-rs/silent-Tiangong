//! 进程内插件的注册编排逻辑。
//!
//! [`register_plugin`] 把一个 [`Plugin`] 的全部能力（工具规格 / 工具覆盖 / Prompt 段落
//! / 工作目录 / 信任模式 / 反馈通道）按统一顺序注册到 [`TurnContext`]，
//! 并返回该插件声明的工具规格，供调用方（worker_loop）做全局编排（同名工具冲突避让、
//! tools 合并等）。
//!
//! 编排顺序（由 worker_loop 在 TurnContext 创建时逐个插件调用）：
//! 1. `set_workspace` — 注入会话工作目录；
//! 2. `set_trust_mode` — 注入会话信任模式解析句柄；
//! 3. `set_feedback_tx` — 注入状态反馈通道（复用 worker 命令通道）；
//! 4. 收集 `tool_specs` 并注册为 `ToolSpecProvider`；
//! 5. 按 spec.name 逐个注册 `ToolOverrideHandler`；
//! 6. 注册 `PromptSectionProvider`。
//!
//! 插件的内部状态初始化（如读取配置、启动后台调度器）在 worker_loop 调用本函数
//! 之前已完成——经由 `on_config_updated`（在收集 specs 前由 core 统一调用）。
//!
//! TurnContext 仅暴露原子能力槽位（`register_tool_*`），全部编排逻辑集中在此。

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::core::command::Command;
use crate::core::plugin::feedback::PluginFeedbackTx;
use crate::model::ToolSpec;
use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};
use crate::turn_context::TurnContext;

use super::trait_def::Plugin;

/// 把单个 [`Plugin`] 的全部能力注册到 [`TurnContext`]，返回该插件声明的工具规格。
///
/// 在 TurnContext 创建时由 `worker_loop` 逐个插件调用。`cmd_tx` 为 worker 的命令通道
/// 发送端，会包装成 [`PluginFeedbackTx`] 注入给插件（复用同一通道，避免新增 channel）。
///
/// 返回值是该插件由 `tool_specs()` 产出的工具规格快照，
/// 供调用方做全局编排（构建 reserved_names 避让同名工具冲突、合并到最终 tools 列表）。
/// 注入与编排顺序见[模块文档]。
///
/// [模块文档]: self
pub(crate) fn register_plugin(
    ctx: &TurnContext,
    plugin: Arc<dyn Plugin>,
    workspace: Option<&Path>,
    cmd_tx: UnboundedSender<Command>,
) -> Vec<ToolSpec> {
    // 1) 注入当前会话工作目录（None 表示无有效 cwd，插件应清空缓存的旧值）
    plugin.set_workspace(workspace);

    // 2) 注入信任模式解析句柄（在收集 tool_specs 前注入，让插件 handler 可读）
    let trust_mode = ctx.permission_gate().trust_mode_handle();
    plugin.set_trust_mode(trust_mode);

    // 3) 注入状态反馈通道（复用 worker 命令通道，clone 给插件持有）
    plugin.set_feedback_tx(PluginFeedbackTx::new(cmd_tx, ctx.turn_usage_sink().clone()));

    // 4) 工具规格：on_config_updated 已由 worker_loop 在调用本函数前统一触发，
    //    插件内部状态已就绪，直接收集 specs。
    let specs = plugin.tool_specs();

    // 5) 工具覆盖：按 spec 中的工具名逐个注册，路由到同一 plugin 的 handle。
    let plugin_as_handler: Arc<dyn ToolOverrideHandler> = plugin.clone();
    for spec in &specs {
        ctx.register_tool_override(&spec.name, plugin_as_handler.clone());
    }

    // 6) Prompt 段落
    let plugin_as_prompt: Arc<dyn PromptSectionProvider> = plugin.clone();
    ctx.register_prompt_section_provider(plugin_as_prompt);

    specs
}
