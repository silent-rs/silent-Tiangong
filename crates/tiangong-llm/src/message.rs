use serde::{Deserialize, Serialize};

use crate::tool::{ToolCall, ToolResult};

/// 统一消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// 预留的图片内容结构。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContent {
    pub mime_type: String,
    pub data: String,
}

/// 统一消息内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageContent {
    Text(String),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Image(ImageContent),
}

/// 统一聊天消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: Vec<MessageContent>,
}

impl ChatMessage {
    pub fn new(role: MessageRole, content: Vec<MessageContent>) -> Self {
        Self { role, content }
    }

    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![MessageContent::Text(text.into())],
        }
    }
}
