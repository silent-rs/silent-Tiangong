//! 统一的外部输出流事件

use serde::{Deserialize, Serialize};

/// 外部输出流事件
///
/// CLI / GUI / Server / Connector 统一消费此类型。
/// 使用 serde tag 序列化，前端可直接用 event.type 判断类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// 文本增量（assistant 回复内容）
    Delta {
        /// 所属消息 ID（前端据此组装到正确的消息）
        message_id: String,
        content: String,
    },
    /// 思考过程增量
    Reasoning {
        /// 所属消息 ID
        message_id: String,
        content: String,
    },
    /// 工具开始执行
    ToolStart { name: String, args_summary: String },
    /// 工具执行结果
    ToolResult {
        name: String,
        ok: bool,
        output: String,
    },
    /// LLM 决定调用工具
    ToolCalls {
        names: Vec<String>,
        /// 本次 LLM 调用的 token 用量
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<crate::TokenUsage>,
    },
    /// 需要用户审批
    ApprovalNeeded {
        request_id: String,
        tool_name: String,
        args_summary: String,
    },
    /// 本轮完成
    Done {
        /// 本轮累计 token 用量
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<crate::TokenUsage>,
    },
    /// 执行出错
    Error { message: String },
    /// LLM 请求重试中
    Retry {
        message: String,
        attempt: u32,
        max_attempts: u32,
    },

    // ===== 多 Worker 并行执行事件 =====
    /// Worker 开始执行
    WorkerStarted {
        worker_id: String,
        worker_label: String,
    },
    /// Worker 流式输出（带 Worker 标识的 Delta）
    WorkerChunk {
        worker_id: String,
        worker_label: String,
        content: String,
    },
    /// Worker 执行完成
    WorkerCompleted {
        worker_id: String,
        worker_label: String,
        success: bool,
    },
    /// 用户消息（Core 收到用户输入后回传，供前端统一渲染）
    UserMessage {
        /// 该用户消息在 session 中的 ID
        message_id: String,
        content: String,
    },
}

/// 带会话标识的流事件
///
/// Core 输出的所有事件都携带 session_id，
/// 消费端（GUI / CLI / Server）可据此路由到正确的会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStreamEvent {
    /// 产生该事件的会话 ID
    pub session_id: String,
    /// 原始流事件
    pub event: StreamEvent,
}
