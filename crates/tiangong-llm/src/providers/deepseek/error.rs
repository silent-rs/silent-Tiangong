use crate::error::LlmError;

pub fn map_deepseek_error(error: tiangong_deepseek::DeepSeekError) -> LlmError {
    match error {
        tiangong_deepseek::DeepSeekError::Config(msg) => LlmError::Configuration(msg),
        tiangong_deepseek::DeepSeekError::Transport(msg) => LlmError::Transport(msg),
        tiangong_deepseek::DeepSeekError::Authentication(msg) => LlmError::Authentication(msg),
        tiangong_deepseek::DeepSeekError::InvalidRequest(msg) => LlmError::InvalidRequest(msg),
        tiangong_deepseek::DeepSeekError::RateLimited(msg) => LlmError::RateLimited(msg),
        tiangong_deepseek::DeepSeekError::Serialization(msg) => LlmError::Serialization(msg),
        tiangong_deepseek::DeepSeekError::Stream(msg) => LlmError::Stream(msg),
        tiangong_deepseek::DeepSeekError::Timeout(_) => LlmError::Timeout(0),
        tiangong_deepseek::DeepSeekError::Api(msg) => LlmError::Provider {
            provider: "deepseek",
            message: msg,
        },
    }
}
