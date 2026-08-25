use crate::error::LlmError;

pub fn map_openai_error(error: &async_openai::error::OpenAIError) -> LlmError {
    let text = format_error_with_sources(error);
    if text.contains("401") {
        return LlmError::Authentication(text);
    }
    if is_rate_limited_text(&text) {
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
    // 只重试真正可能恢复的传输层故障（连接被重置/断开、DNS 抖动等）。
    // reqwest 的 Display 只有 "error sending request for url (...)"，
    // 底层原因藏在 source 链里，无法靠字符串匹配识别。
    // 构建失败、超时、响应体解析失败、重定向错误都是确定性失败，重试无益：
    // 格式错误的响应重发只会重复计费并推迟报错。
    if let async_openai::error::OpenAIError::Reqwest(e) = err
        && !e.is_builder()
        && !e.is_timeout()
        && !e.is_decode()
        && !e.is_redirect()
    {
        return true;
    }
    let text = format_error_with_sources(err);
    is_rate_limited_text(&text)
        || text.contains("500 Internal Server Error")
        || text.contains("502 Bad Gateway")
        || text.contains("503 Service Unavailable")
        || text.contains("504 Gateway Timeout")
        || text.contains("connection reset")
        || text.contains("connection refused")
}

/// 拼接错误与其 source 链：reqwest 的 Display 不含底层原因，
/// 需要手动展开才能把 "connection reset by peer" 这类信息透给用户。
fn format_error_with_sources(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(err) = source {
        text.push_str(": ");
        text.push_str(&err.to_string());
        source = err.source();
    }
    text
}

fn is_rate_limited_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains("429")
        || text.contains("529")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("overloaded_error")
}
