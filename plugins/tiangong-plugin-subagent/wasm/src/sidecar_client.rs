//! sidecar 调用封装：工具透传与生命周期钩子转发共用。
//!
//! subagent 的 sidecar dispatch 对全部工具操作返回统一的 ToolOutcome 形状
//! （{ok, summary, stdout?, exit_code}），因此工具调用直接透传操作名与参数
//! JSON，不逐工具定义请求类型。

use crate::bindings::tiangong::plugin::sidecar;

#[derive(Debug)]
pub enum ClientError {
    NotConfigured,
    Unavailable(String),
    Timeout,
    PermissionDenied,
    ProtocolMismatch(String),
    Internal(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => formatter.write_str("sidecar 未配置"),
            Self::Unavailable(message) => write!(formatter, "sidecar 不可用: {message}"),
            Self::Timeout => formatter.write_str("sidecar 请求超时"),
            Self::PermissionDenied => formatter.write_str("sidecar 权限不足"),
            Self::ProtocolMismatch(message) => {
                write!(formatter, "sidecar 协议不兼容: {message}")
            }
            Self::Internal(message) => write!(formatter, "sidecar 内部错误: {message}"),
        }
    }
}

/// 透传调用：operation + 参数 JSON 文本 → sidecar 响应 JSON 文本。
pub fn invoke_raw(operation: &str, payload: &str) -> Result<String, ClientError> {
    sidecar::invoke(operation, payload).map_err(map_transport_error)
}

fn map_transport_error(error: sidecar::SidecarError) -> ClientError {
    match error {
        sidecar::SidecarError::NotConfigured => ClientError::NotConfigured,
        sidecar::SidecarError::Unavailable(message) => ClientError::Unavailable(message),
        sidecar::SidecarError::Timeout => ClientError::Timeout,
        sidecar::SidecarError::PermissionDenied => ClientError::PermissionDenied,
        sidecar::SidecarError::ProtocolMismatch(message) => ClientError::ProtocolMismatch(message),
        sidecar::SidecarError::Internal(message) => ClientError::Internal(message),
    }
}
