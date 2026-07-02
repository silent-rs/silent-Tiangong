//! 插件 → core 的状态反馈通道。
//!
//! 复用 worker_loop 已有的 `cmd_tx: tokio_mpsc::UnboundedSender<Command>` 通道：
//! 插件通过 [`PluginFeedbackTx`] 投递 [`PluginFeedback`]，core 内部转换为
//! [`Command::InjectTool`](crate::core::command::Command::InjectTool)，由 worker 统一
//! 注入到 session（与 `Command::InjectTool` 的处理路径完全一致）。
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
