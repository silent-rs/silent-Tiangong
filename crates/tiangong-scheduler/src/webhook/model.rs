use serde::{Deserialize, Serialize};

/// Webhook 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    pub id: String,
    pub name: String,
    pub description: String,
    /// 可选，关联已有 session 复用上下文；为空时触发自动创建新 session
    #[serde(default)]
    pub session_id: Option<String>,
    /// 触发时构造给 LLM 的任务描述
    pub payload: String,
    /// 签名密钥（可选，配置后需在请求头 X-Webhook-Signature 中传入）
    #[serde(default)]
    pub secret: Option<String>,
    /// 是否启用
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建 Webhook 请求
#[derive(Debug, Deserialize)]
pub struct CreateWebhookRequest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub payload: String,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
}

/// 更新 Webhook 请求（所有字段可选）
#[derive(Debug, Default, Deserialize)]
pub struct UpdateWebhookRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub payload: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Webhook 执行记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRun {
    pub id: String,
    pub webhook_id: String,
    /// 本次执行使用的 session_id
    pub session_id: String,
    pub status: WebhookRunStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub result_summary: Option<String>,
}

/// Webhook 执行状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebhookRunStatus {
    Running,
    Succeeded,
    Failed,
}
