use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum DeepSeekError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("authentication failed: {0}")]
    Authentication(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("stream error: {0}")]
    Stream(String),

    #[error("API error: {0}")]
    Api(String),
}

impl DeepSeekError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::RateLimited(_))
    }
}
