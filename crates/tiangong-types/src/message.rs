//! 消息类型

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 消息角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// 媒体类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    File,
}

/// 会话消息中的结构化媒体资源
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub kind: MediaKind,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
}

/// 对话消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    #[serde(default)]
    pub reasoning_content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_signature: Option<String>,
    /// 多 Worker 模式下标识消息所属 Worker
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaAsset>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<MessageToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tool_result_is_error: bool,
    /// 表示从当前消息及以前的历史已被压缩摘要覆盖。
    #[serde(default, skip_serializing_if = "is_false")]
    pub compact: bool,
    pub created_at: String,
}

impl Message {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            id: scru128::new().to_string(),
            role,
            content: content.into(),
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
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
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            created_at: now_text(),
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// 当前本地时间文本
pub fn now_text() -> String {
    chrono::Local::now().naive_local().to_string()
}
