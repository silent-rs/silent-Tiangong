//! 插件 → core 的语义反馈通道。
//!
//! 复用 worker_loop 已有的 `cmd_tx: tokio_mpsc::UnboundedSender<Command>` 通道，
//! 让插件向 core 投递**语义事件**，由 core 统一决定如何处理（是否累加 usage、是否
//! 注入 session、是否转发为流事件）。
//!
//! [`PluginFeedback`] 为语义枚举，目前支持：
//!
//! - **会话注入**（[`PluginFeedback::InjectTool`]）：插件产生需要进入对话上下文的
//!   外部事件（如浏览器页面变化、终端用户操作），core 转为 [`Command::InjectTool`]
//!   注入 session（以 tool result 形式出现在对话中）。
//! - **用量上报**（[`PluginFeedback::TokenUsage`]）：插件在工具执行中产生了一笔
//!   LLM token 消耗（如 `analyze_attachment` 调用 multimodal 子模型），core 负责
//!   累加到本轮 `accumulated_usage`、统一发送 `StreamEvent::TokenUsage`，并确保
//!   最终 `Done.usage` 包含该消耗。
//!
//! 插件只描述事实，core 决策处理方式。这样插件无需依赖 `tiangong-app` 的
//! `ToolInjection` 类型，也无需各自持有 `stream_tx`——拿到注入的 [`PluginFeedbackTx`]
//! 后即可直接投递浏览器页面变化、终端用户操作、子调用 token 用量等事件。

use tokio::sync::mpsc::UnboundedSender;

use crate::core::command::Command;

/// 插件向 core 投递的语义反馈。
///
/// 插件只描述“发生了什么”，由 core 统一决定“如何处理”（累加 usage / 注入 session /
/// 转发流事件）。详见[模块文档]。
///
/// [模块文档]: self
#[derive(Debug, Clone)]
pub enum PluginFeedback {
    /// 注入一条工具结果到对话上下文（以 tool result 形式出现在对话中）。
    ///
    /// `tool_name` 为来源工具名（伪造 tool_call 的 name 字段，如 `plugin_injection`），
    /// `payload` 为结构化内容（JSON）。core 按 `tool_name` 决定呈现格式，并保留
    /// 结构化数据供去重等逻辑使用（与 [`crate::agent_input::ToolInput`] 协议一致）。
    InjectTool {
        tool_name: String,
        payload: serde_json::Value,
    },
    /// 上报一笔插件内部产生的 LLM token 用量（如 `analyze_attachment` 调用 multimodal
    /// 子模型）。
    ///
    /// core 负责：累加到本轮 `accumulated_usage`、统一发送 `StreamEvent::TokenUsage`、
    /// 确保最终 `Done.usage` 包含该消耗（保持成本统计与上下文压缩判断一致）。
    /// `source` 用于标识来源（如工具名），`agent_id` 标识归属 agent（None 表示主对话）。
    TokenUsage {
        usage: tiangong_types::TokenUsage,
        source: String,
        agent_id: Option<String>,
    },
}

impl PluginFeedback {
    /// 便捷构造会话注入反馈。
    pub fn inject_tool(tool_name: impl Into<String>, payload: serde_json::Value) -> Self {
        Self::InjectTool {
            tool_name: tool_name.into(),
            payload,
        }
    }

    /// 便捷构造用量上报反馈。
    pub fn token_usage(usage: tiangong_types::TokenUsage, source: impl Into<String>) -> Self {
        Self::TokenUsage {
            usage,
            source: source.into(),
            agent_id: None,
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
    /// 投递一条语义反馈。通道关闭（worker 已退出）时静默丢弃，不报错。
    pub fn send(&self, feedback: PluginFeedback) {
        let cmd = match feedback {
            PluginFeedback::InjectTool { tool_name, payload } => {
                Command::InjectTool { tool_name, payload }
            }
            PluginFeedback::TokenUsage {
                usage,
                source,
                agent_id,
            } => Command::ReportPluginUsage {
                usage,
                source,
                agent_id,
            },
        };
        let _ = self.tx.send(cmd);
    }

    /// 注入一条工具结果到对话上下文（`tool_name` + JSON payload）。
    pub fn inject_tool(&self, tool_name: impl Into<String>, payload: serde_json::Value) {
        self.send(PluginFeedback::inject_tool(tool_name, payload));
    }

    /// 上报一笔插件内部产生的 LLM token 用量。
    ///
    /// core 会将其累加到本轮 `accumulated_usage` 并统一上报，确保成本统计、上下文
    /// 压缩判断与 `Done.usage` 都包含该消耗。通道关闭时静默丢弃。
    pub fn report_token_usage(&self, usage: tiangong_types::TokenUsage, source: impl Into<String>) {
        self.send(PluginFeedback::token_usage(usage, source));
    }

    /// 投递一条流事件（转发到 worker 的 `stream_tx`）。
    ///
    /// 仅向 UI 推送实时事件（如 `MemoryRecallStart` / `MemoryRecallProgress` /
    /// `MemoryRecallDone`），不进入对话历史，也不累加任何 usage。通道关闭时静默丢弃。
    ///
    /// 注意：用于上报 LLM token 用量时应改用 [`report_token_usage`](Self::report_token_usage)，
    /// 后者能让 core 正确记账；直接用本方法转发 `StreamEvent::TokenUsage` 只会让前端
    /// 看到事件而不会计入本轮统计。
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
