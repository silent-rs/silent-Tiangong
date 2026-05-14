//! 统一的外部输出流事件

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        ok: bool,
        /// 给 UI/外部消费者展示的输出，可能被截断。
        output: String,
        /// Rust 内部落盘使用的完整输出，不序列化给前端或远端消费者。
        #[serde(default, skip)]
        full_output: Option<String>,
    },
    /// LLM 决定调用工具
    ToolCalls {
        message_id: String,
        names: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        calls: Vec<StreamToolCall>,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media: Vec<crate::MediaAsset>,
    },

    // ===== 记忆检索事件 =====
    /// 记忆检索开始
    MemoryRecallStart {
        /// 检索策略描述（如 "keyword" / "semantic" / "hybrid:0.6" / "skip"）
        strategy: String,
    },
    /// 记忆检索进度更新
    MemoryRecallProgress {
        /// 当前阶段描述
        phase: String,
    },
    /// 记忆检索完成
    MemoryRecallDone {
        /// 命中条数
        hit_count: usize,
        /// 命中摘要列表（每项为 "标题: 摘要"）
        hits: Vec<MemoryRecallHitSummary>,
    },

    // ===== 多智能体团队事件 =====
    /// Agent 创建
    AgentCreated {
        agent_id: String,
        role: String,
        label: String,
        lifecycle: String,
    },
    /// Agent 状态变更
    AgentStatusChanged {
        agent_id: String,
        label: String,
        status: String,
    },
    /// Agent 向用户直接推送的通知
    AgentNotification {
        agent_id: String,
        agent_label: String,
        content: String,
        level: String,
    },
    /// Agent 间消息
    AgentMessage {
        from_agent_id: String,
        from_agent_label: String,
        to_agent_id: String,
        to_agent_label: String,
        content: String,
    },
    /// Agent 执行输出快照
    AgentOutput {
        agent_id: String,
        agent_role: String,
        agent_label: String,
        messages: Vec<crate::Message>,
    },
    /// 文件锁变更
    FileLockChanged {
        path: String,
        holder_agent_id: Option<String>,
        holder_agent_label: Option<String>,
        action: String,
    },
    /// 上下文已压缩/清理
    ContextCompressed {
        action: String,
        summary_up_to: usize,
        remaining_messages: usize,
    },
}

/// 记忆检索命中项摘要（前端展示用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecallHitSummary {
    pub title: String,
    pub summary: String,
    pub score: f64,
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
