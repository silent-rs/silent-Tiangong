use thiserror::Error;

/// LLM provider 统一错误模型。
#[derive(Debug, Error, Clone)]
pub enum LlmError {
    #[error("配置错误：{0}")]
    Configuration(String),

    #[error("传输错误：{0}")]
    Transport(String),

    #[error("请求超时：{0}ms")]
    Timeout(u64),

    #[error("请求被限流：{0}")]
    RateLimited(String),

    #[error("认证失败：{0}")]
    Authentication(String),

    #[error("请求无效：{0}")]
    InvalidRequest(String),

    #[error("序列化失败：{0}")]
    Serialization(String),

    #[error("流式处理失败：{0}")]
    Stream(String),

    #[error("{provider} provider 错误：{message}")]
    Provider {
        provider: &'static str,
        message: String,
    },

    #[error("不支持的能力：{0}")]
    UnsupportedFeature(String),
}

impl LlmError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport(_) | Self::Timeout(_) | Self::RateLimited(_) | Self::Stream(_)
        )
    }
}
