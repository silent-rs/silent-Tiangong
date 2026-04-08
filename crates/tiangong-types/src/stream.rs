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
    Delta { content: String },
    /// 思考过程增量
    Reasoning { content: String },
    /// 工具开始执行
    ToolStart { name: String, summary: String },
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
}
