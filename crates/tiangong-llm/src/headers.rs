use crate::error::LlmError;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::collections::BTreeMap;

pub(crate) fn resolve_headers(
    headers: &BTreeMap<String, String>,
    session_id: &str,
) -> Result<HeaderMap, LlmError> {
    let mut resolved = HeaderMap::new();
    resolved.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_static(concat!("tiangong/", env!("CARGO_PKG_VERSION"))),
    );
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
