//! 插件 → core 的状态反馈通道。
//!
//! 复用 worker_loop 已有的 `cmd_tx: tokio_mpsc::UnboundedSender<Command>` 通道，
//! 支持两类投递：
//!
//! - **会话注入**：通过 [`PluginFeedbackTx::send`] / [`PluginFeedbackTx::send_payload`]
//!   投递 [`PluginFeedback`]，core 转为 [`Command::InjectTool`]，由 worker 注入到
//!   session（以 tool result 形式出现在对话中）。适合浏览器页面变化、终端用户操作等
//!   需要进入对话上下文的外部事件。
//!
//! - **流事件转发**：通过 [`PluginFeedbackTx::send_stream_event`] 投递
//!   [`StreamEvent`](tiangong_types::StreamEvent)，core 转为
//!   [`Command::EmitStreamEvent`](crate::core::command::Command::EmitStreamEvent)，
//!   worker 直接转发到 `stream_tx`。适合插件工具需要向 UI 发实时进度/状态事件
//!   （如 `recall_memory` 的 Start/Progress/Done），无需各自持有 stream_tx。
//!
//! 这样插件无需依赖 `tiangong-app` 的 `ToolInjection` 类型，也无需在 main.rs 里
//! 用 Tauri 事件 `listen` 做胶水——拿到注入的 [`PluginFeedbackTx`] 后即可直接投递
//! 浏览器页面变化、终端用户操作等外部事件。

use tokio::sync::mpsc::UnboundedSender;

use crate::core::command::Command;

/// 插件向 core 投递的状态反馈（外部事件，如浏览器页面变化、终端用户操作）。
///
/// 与 [`Command::InjectTool`] 同构：`tool_name` + 结构化 `payload`。
/// core 的 worker 把它注入到 session，以 tool result 形式出现在对话中。
///
/// `payload` 返回 JSON 而非文本，让 worker 侧根据 `tool_name` 决定呈现格式，
/// 同时保留结构化数据供去重等逻辑使用（与 [`crate::agent_input::ToolInput`] 协议一致）。
#[derive(Debug, Clone)]
pub struct PluginFeedback {
    /// 工具名（伪造 tool_call 的 name 字段，如 `plugin_injection`）。
    pub tool_name: String,
    /// 注入到对话的结构化内容（JSON）。
    pub payload: serde_json::Value,
}

impl PluginFeedback {
    /// 便捷构造。
    pub fn new(tool_name: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            payload,
        }
    }
}

/// 插件状态反馈通道的发送端。
///
/// 封装 core 内部的 `cmd_tx`（[`UnboundedSender<Command>`]），插件持有 clone 后即可
/// 投递 [`PluginFeedback`]，无需感知 core 私有的 [`Command`] 类型。
///
/// `clone` 与底层 `UnboundedSender::clone` 同义，clone 多份指向同一接收端。
#[derive(Clone)]
pub struct PluginFeedbackTx {
    tx: UnboundedSender<Command>,
}

impl PluginFeedbackTx {
    /// 投递一条插件反馈。通道关闭（worker 已退出）时静默丢弃，不报错。
    pub fn send(&self, feedback: PluginFeedback) {
        let _ = self.tx.send(Command::InjectTool {
            tool_name: feedback.tool_name,
            payload: feedback.payload,
        });
    }

    /// 便捷投递：`tool_name` + JSON payload。
    pub fn send_payload(&self, tool_name: impl Into<String>, payload: serde_json::Value) {
        self.send(PluginFeedback::new(tool_name, payload));
    }

    /// 投递一条流事件（转发到 worker 的 `stream_tx`）。
    ///
    /// 与 [`send`](Self::send) 的区别：`send` 把内容作为 tool result 注入对话上下文，
    /// 而 `send_stream_event` 仅向 UI 推送实时事件（如 `MemoryRecallStart` /
    /// `MemoryRecallProgress` / `MemoryRecallDone`），不进入对话历史。通道关闭时静默丢弃。
    pub fn send_stream_event(&self, event: tiangong_types::StreamEvent) {
        let _ = self.tx.send(Command::EmitStreamEvent(event));
    }

    /// 通道是否已关闭（worker 已退出，无法再投递）。
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// 从 core 内部的 `cmd_tx` 构造反馈通道（仅 core 可调用）。
impl From<UnboundedSender<Command>> for PluginFeedbackTx {
    fn from(tx: UnboundedSender<Command>) -> Self {
        Self { tx }
    }
}
