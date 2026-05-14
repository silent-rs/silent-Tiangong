use serde::{Deserialize, Serialize};

/// 消息优先级
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePriority {
    Normal,
    Urgent,
}

/// Agent 间消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    /// 消息 ID
    pub id: String,
    /// 发送方 agent_id
    pub from: String,
    /// 接收方 agent_id（广播时为 "all"）
    pub to: String,
    /// 消息内容
    pub content: String,
    /// 优先级
    pub priority: MessagePriority,
    /// 时间戳
    pub created_at: String,
}
