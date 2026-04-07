//! 消息类型

use serde::{Deserialize, Serialize};

/// 消息角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// 对话消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    #[serde(default)]
    pub reasoning_content: String,
    /// 多 Worker 模式下标识消息所属 Worker
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    pub created_at: String,
}

impl Message {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            id: scru128::new().to_string(),
            role,
            content: content.into(),
            reasoning_content: String::new(),
            worker_id: None,
            created_at: now_text(),
        }
    }

    pub fn with_reasoning(
        role: MessageRole,
        content: impl Into<String>,
        reasoning: impl Into<String>,
    ) -> Self {
        Self {
            id: scru128::new().to_string(),
            role,
            content: content.into(),
            reasoning_content: reasoning.into(),
            worker_id: None,
            created_at: now_text(),
        }
    }
}

/// 当前本地时间文本
pub fn now_text() -> String {
    chrono::Local::now().naive_local().to_string()
}
