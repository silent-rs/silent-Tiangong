//! 进程内插件的注册编排逻辑。
//!
//! [`register_plugin`] 把一个 [`Plugin`] 的全部能力（工具规格 / 工具覆盖 / Prompt 段落
//! / 外部能力 / 工作目录 / 信任模式 / 反馈通道）按统一顺序注册到 [`RuntimeEngine`]。
//!
//! 编排顺序（由 worker_loop 在 engine 创建/重建时逐个插件调用）：
//! 1. `set_workspace` — 注入会话工作目录；
//! 2. `set_trust_mode` — 注入共享信任模式引用（在 `register` 前，让 `register` 内可读）；
//! 3. `set_feedback_tx` — 注入状态反馈通道（复用 worker 命令通道）；
//! 4. `set_memory_handle` — 注入记忆句柄（记忆系统启用时为 Some）；
//! 5. 收集 `tool_specs` 并注册为 `ToolSpecProvider`；
//! 6. 按 spec.name 逐个注册 `ToolOverrideHandler`；
//! 7. 注册 `PromptSectionProvider`；
//! 8. 调 `Plugin::register` 让插件注入外部能力（PageFetcher / TerminalProvider 等）。
//!
//! engine 仅暴露原子能力槽位（`register_tool_*`），全部编排逻辑集中在此。

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::core::command::Command;
use crate::core::plugin::feedback::PluginFeedbackTx;
use crate::runtime::RuntimeEngine;
use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

use super::trait_def::Plugin;

/// 把单个 [`Plugin`] 的全部能力注册到 [`RuntimeEngine`]。
///
/// 在 engine 创建/重建时由 `worker_loop` 逐个插件调用。`cmd_tx` 为 worker 的命令通道
/// 发送端，会包装成 [`PluginFeedbackTx`] 注入给插件（复用同一通道，避免新增 channel）。
/// `memory_handle` 为记忆系统句柄（记忆系统启用时为 `Some`，会 clone 一份注入给插件）。
/// 所有注入顺序见[模块文档]。
///
/// [模块文档]: self
pub(crate) fn register_plugin(
    engine: &RuntimeEngine,
    plugin: Arc<dyn Plugin>,
    workspace: Option<&Path>,
    cmd_tx: UnboundedSender<Command>,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
) {
    // 1) 注入当前会话工作目录（插件可覆写 set_workspace 感知）
    if let Some(ws) = workspace {
        plugin.set_workspace(ws);
    }

    // 2) 注入共享信任模式引用（在 register 之前，让 register 内可读到）
    let shared_trust = engine.permission_gate().shared_trust_mode_ref();
    plugin.set_trust_mode(shared_trust);

    // 3) 注入状态反馈通道（复用 worker 命令通道，clone 给插件持有）
    plugin.set_feedback_tx(PluginFeedbackTx::from(cmd_tx));

    // 4) 注入记忆句柄（MemoryHandle 内部为 Arc，clone 一份给插件跨 turn 复用）
    plugin.set_memory_handle(memory_handle.cloned());

    // 5) 工具规格：plugin 本身即 ToolSpecProvider
    let specs = plugin.tool_specs();
    let plugin_as_spec: Arc<dyn ToolSpecProvider> = plugin.clone();
    engine.register_tool_spec_provider(plugin_as_spec);

    // 6) 工具覆盖：按 spec 中的工具名逐个注册，路由到同一 plugin 的 handle
    let plugin_as_handler: Arc<dyn ToolOverrideHandler> = plugin.clone();
    for spec in &specs {
        engine.register_tool_override(&spec.name, plugin_as_handler.clone());
    }

    // 7) Prompt 段落
    let plugin_as_prompt: Arc<dyn PromptSectionProvider> = plugin.clone();
    engine.register_prompt_section_provider(plugin_as_prompt);

    // 8) 让插件注入外部能力（PageFetcher / TerminalProvider 等）
    plugin.register(engine);
}
