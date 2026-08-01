use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryErrorCode {
    Disabled,
    InvalidRequest,
    NotFound,
    Conflict,
    ModelUnavailable,
    StorageUnavailable,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryError {
    pub code: MemoryErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}
