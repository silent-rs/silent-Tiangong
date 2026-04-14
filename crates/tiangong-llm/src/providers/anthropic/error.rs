use async_anthropic::errors::AnthropicError;

use crate::error::LlmError;

pub fn map_anthropic_error(error: AnthropicError) -> LlmError {
    match error {
        AnthropicError::NetworkError(err) => {
            if err.is_timeout() {
                LlmError::Timeout(0)
            } else {
                LlmError::Transport(err.to_string())
            }
        }
        AnthropicError::BadRequest(message) => LlmError::InvalidRequest(message),
        AnthropicError::ApiError(message) => LlmError::RateLimited(message),
        AnthropicError::Unauthorized => {
            LlmError::Authentication("请检查 Anthropic API Key".to_string())
        }
        AnthropicError::DeserializationError(err) => LlmError::Serialization(err.to_string()),
        AnthropicError::Unknown(message) => LlmError::Provider {
            provider: "anthropic",
            message,
        },
        AnthropicError::UnexpectedError => LlmError::Provider {
            provider: "anthropic",
            message: "unexpected error".to_string(),
        },
        AnthropicError::StreamError(err) => LlmError::Stream(err.to_string()),
    }
}
