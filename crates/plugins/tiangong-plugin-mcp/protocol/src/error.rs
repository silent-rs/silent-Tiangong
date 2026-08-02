use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpErrorCode {
    Disabled,
    InvalidRequest,
    NotFound,
    Conflict,
    TransportUnavailable,
    ServerUnhealthy,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpError {
    pub code: McpErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}
