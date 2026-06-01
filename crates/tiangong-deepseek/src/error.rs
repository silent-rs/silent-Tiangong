use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum DeepSeekError {
    #[error("配置错误：{0}")]
    Config(String),

    #[error("传输错误：{0}")]
    Transport(String),

    #[error("认证失败：{0}")]
    Authentication(String),

    #[error("请求无效：{0}")]
    InvalidRequest(String),

    #[error("请求被限流：{0}")]
    RateLimited(String),

    #[error("序列化失败：{0}")]
    Serialization(String),

    #[error("流式处理失败：{0}")]
    Stream(String),

    #[error("DeepSeek API 错误：{0}")]
    Api(String),
}

impl DeepSeekError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::RateLimited(_))
    }
}
