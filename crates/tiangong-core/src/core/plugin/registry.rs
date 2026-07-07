//! 进程内插件的注册编排逻辑。
//!
//! [`register_plugin`] 把一个 [`Plugin`] 的全部能力（工具规格 / 工具覆盖 / Prompt 段落
//! / 外部能力 / 工作目录 / 信任模式 / 反馈通道）按统一顺序注册到 [`RuntimeEngine`]，
//! 并返回该插件声明的工具规格，供调用方（worker_loop）做全局编排（MCP 冲突避让、
//! tools 合并等）。
//!
//! 编排顺序（由 worker_loop 在 engine 创建/重建时逐个插件调用）：
//! 1. `set_workspace` — 注入会话工作目录；
//! 2. `set_trust_mode` — 注入共享信任模式引用；
//! 3. `set_feedback_tx` — 注入状态反馈通道（复用 worker 命令通道）；
//! 4. `Plugin::register` — 让插件初始化内部状态或注入 engine 依赖
//!    （如克隆 models_config、注入 PageFetcher / TerminalProvider 等）。必须在收集
//!    tool_specs 前，确保插件已就绪；
//! 5. 收集 `tool_specs` 并注册为 `ToolSpecProvider`；
//! 6. 按 spec.name 逐个注册 `ToolOverrideHandler`（基于 register 之后的正确 specs）；
//! 7. 注册 `PromptSectionProvider`。
//!
//! 步骤 4-6 的顺序至关重要：`register` 先执行，确保插件内部状态就绪，
//! 此后 `tool_specs()` 读到的才是最新值，override handler 才会注册到正确的工具名上。
//!
//! engine 仅暴露原子能力槽位（`register_tool_*`），全部编排逻辑集中在此。

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::core::command::Command;
use crate::core::plugin::feedback::PluginFeedbackTx;
use crate::model::ToolSpec;
use crate::runtime::RuntimeEngine;
use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

use super::trait_def::Plugin;

/// 把单个 [`Plugin`] 的全部能力注册到 [`RuntimeEngine`]，返回该插件声明的工具规格。
///
/// 在 engine 创建/重建时由 `worker_loop` 逐个插件调用。`cmd_tx` 为 worker 的命令通道
/// 发送端，会包装成 [`PluginFeedbackTx`] 注入给插件（复用同一通道，避免新增 channel）。
///
/// 返回值是该插件经 `register` 初始化后、由 `tool_specs()` 产出的工具规格快照，
/// 供调用方做全局编排（构建 reserved_names 避让 MCP 同名工具、合并到最终 tools 列表）。
/// 注入与编排顺序见[模块文档]。
///
/// [模块文档]: self
pub(crate) fn register_plugin(
    engine: &RuntimeEngine,
    plugin: Arc<dyn Plugin>,
    workspace: Option<&Path>,
    cmd_tx: UnboundedSender<Command>,
) -> Vec<ToolSpec> {
    // 1) 注入当前会话工作目录（None 表示无有效 cwd，插件应清空缓存的旧值）
    plugin.set_workspace(workspace);

    // 2) 注入共享信任模式引用（在 register 之前，让 register 内可读）
    let shared_trust = engine.permission_gate().shared_trust_mode_ref();
    plugin.set_trust_mode(shared_trust);

    // 3) 注入状态反馈通道（复用 worker 命令通道，clone 给插件持有）
    plugin.set_feedback_tx(PluginFeedbackTx::new(
        cmd_tx,
        engine.turn_usage_sink().clone(),
    ));

    // 4) 让插件初始化内部状态并注入外部能力（如克隆 models_config 供 handler 使用）。
    //    必须在收集 tool_specs 前，确保插件已就绪。
    plugin.register(engine);

    // 5) 工具规格：register 之后收集，确保读到插件初始化后的最新值。
    let specs = plugin.tool_specs();
    let plugin_as_spec: Arc<dyn ToolSpecProvider> = plugin.clone();
    engine.register_tool_spec_provider(plugin_as_spec);

    // 6) 工具覆盖：按 spec 中的工具名逐个注册，路由到同一 plugin 的 handle。
    let plugin_as_handler: Arc<dyn ToolOverrideHandler> = plugin.clone();
    for spec in &specs {
        engine.register_tool_override(&spec.name, plugin_as_handler.clone());
    }

    // 7) Prompt 段落
    let plugin_as_prompt: Arc<dyn PromptSectionProvider> = plugin.clone();
    engine.register_prompt_section_provider(plugin_as_prompt);

    specs
}
