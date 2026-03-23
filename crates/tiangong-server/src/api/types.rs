use serde::{Deserialize, Serialize};

/// POST /api/v1/chat 请求体
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    /// 可选的会话 ID，为空时使用当前活跃会话
    pub session_id: Option<String>,
    /// 用户消息内容
    pub message: String,
}

/// POST /api/v1/chat 响应体
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub session_id: String,
    pub response: String,
}

/// 会话摘要（列表项）
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub message_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

/// 消息概要
#[derive(Debug, Serialize)]
pub struct MessageSummary {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}
