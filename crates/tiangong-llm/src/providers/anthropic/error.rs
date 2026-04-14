use tiangong_anthropic::AnthropicError;

use crate::error::LlmError;

pub fn map_anthropic_error(error: AnthropicError) -> LlmError {
    match error {
        AnthropicError::Config(err) => LlmError::Configuration(err),
        AnthropicError::Transport(err) => LlmError::Transport(err),
        AnthropicError::Authentication(err) => LlmError::Authentication(err),
        AnthropicError::InvalidRequest(err) => LlmError::InvalidRequest(err),
        AnthropicError::RateLimited(err) => LlmError::RateLimited(err),
        AnthropicError::Serialization(err) => LlmError::Serialization(err),
        AnthropicError::Stream(err) => LlmError::Stream(err),
        AnthropicError::Api(err) => LlmError::Provider {
            provider: "anthropic",
            message: err,
        },
    }
}
