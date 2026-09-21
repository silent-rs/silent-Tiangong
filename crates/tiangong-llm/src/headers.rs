use crate::error::LlmError;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::collections::BTreeMap;

/// 天工对外 LLM 请求统一 User-Agent 标识。
pub(crate) const TIANGONG_USER_AGENT: &str = concat!("tiangong/", env!("CARGO_PKG_VERSION"));

/// 构造仅含天工 User-Agent 标注的默认请求头。
///
/// 用于无用户自定义头配置的 LLM 端点（embedding、rerank 等），
/// 保证所有对外 LLM 请求都携带天工标识。
pub(crate) fn tiangong_default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_static(TIANGONG_USER_AGENT),
    );
    headers
}

pub(crate) fn resolve_headers(
    headers: &BTreeMap<String, String>,
    session_id: &str,
) -> Result<HeaderMap, LlmError> {
    let mut resolved = tiangong_default_headers();
    for (name, value) in headers {
        let header = HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|_| LlmError::Configuration(format!("请求头名称无效：{name}")))?;
        let value = value.replace("${session_id}", session_id);
        let mut value = HeaderValue::from_str(&value)
            .map_err(|_| LlmError::Configuration(format!("请求头 {name} 的值无效")))?;
        value.set_sensitive(true);
        resolved.insert(header, value);
    }
    Ok(resolved)
}
