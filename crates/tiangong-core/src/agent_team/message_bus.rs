use serde::{Deserialize, Serialize};
use tiangong_types::ContentBlock;

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

/// 投递到子 Agent 收件箱的完整消息。
///
/// `AgentMessage` 保留团队消息自身的文本与来源；其余内容块和目标 Session 的
/// 用户消息 ID 随投递一起保存，避免空闲 Agent 被唤醒时丢失已准备好的输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInboxEntry {
    pub message: AgentMessage,
    pub additional_content: Vec<ContentBlock>,
    pub session_message_id: Option<String>,
}

impl AgentInboxEntry {
    pub fn plain(message: AgentMessage) -> Self {
        Self {
            message,
            additional_content: Vec::new(),
            session_message_id: None,
        }
    }
}

impl std::ops::Deref for AgentInboxEntry {
    type Target = AgentMessage;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}
