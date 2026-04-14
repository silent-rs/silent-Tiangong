use crate::error::LlmError;

pub fn map_openai_error(error: &async_openai::error::OpenAIError) -> LlmError {
    let text = error.to_string();
    if text.contains("401") {
        return LlmError::Authentication(text);
    }
    if text.contains("429") || text.to_ascii_lowercase().contains("rate limit") {
        return LlmError::RateLimited(text);
    }
    if text.contains("400") {
        return LlmError::InvalidRequest(text);
    }
    if text.contains("timeout") {
        return LlmError::Timeout(0);
    }
    LlmError::Transport(text)
}

pub fn is_retryable_openai_error(err: &async_openai::error::OpenAIError) -> bool {
    let text = err.to_string();
    text.contains("429")
        || text.contains("500 Internal Server Error")
        || text.contains("502 Bad Gateway")
        || text.contains("503 Service Unavailable")
        || text.contains("504 Gateway Timeout")
        || text.contains("connection reset")
        || text.contains("connection refused")
        || text.to_ascii_lowercase().contains("rate limit")
}
