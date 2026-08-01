//! WASM 插件运行时与配套 sidecar 共用的通用协议。
//!
//! 这里只定义传输、请求信封和运行状态，不包含任何具体插件的业务类型。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 通用 sidecar 协议版本。
pub const PROTOCOL_VERSION: &str = "0.1.0";
/// 由运行时发起的健康检查操作。
pub const HANDSHAKE_OPERATION: &str = "runtime.handshake";

/// 运行时发送给 sidecar 的请求信封。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub protocol_version: String,
    pub request_id: String,
    pub operation: String,
    pub payload: serde_json::Value,
}

impl Request {
    pub fn new(operation: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: next_request_id(),
            operation: operation.into(),
            payload,
        }
    }
}

/// sidecar 返回给运行时的响应信封。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub protocol_version: String,
    pub request_id: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default)]
    pub retryable: bool,
}

impl Response {
    pub fn success(request_id: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: request_id.into(),
            success: true,
            payload: Some(payload),
            error_code: None,
            error_message: None,
            retryable: false,
        }
    }

    pub fn error(
        request_id: impl Into<String>,
        code: ErrorCode,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: request_id.into(),
            success: false,
            payload: None,
            error_code: Some(code),
            error_message: Some(message.into()),
            retryable,
        }
    }
}

/// 与具体插件无关的通用错误码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unavailable,
    Timeout,
    PayloadTooLarge,
    ProtocolMismatch,
    PermissionDenied,
    BadRequest,
    ServiceDisabled,
    ServiceError,
}

/// 通用握手响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub plugin_id: String,
    pub plugin_version: String,
    pub sidecar_version: String,
    pub protocol_version: String,
    #[serde(default)]
    pub business_protocol: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub instance_id: String,
    pub status: ServiceStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Ready,
    Initializing,
    Degraded,
}

/// Endpoint 发现信息，由 sidecar 写入本地文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcEndpoint {
    pub service: String,
    pub host: String,
    pub port: u16,
    pub pid: u32,
    pub token: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub request_id: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub request_id: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcAuth {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcFrame {
    Auth(IpcAuth),
    Request(IpcRequest),
    Progress { request_id: String, message: String },
    Response(IpcResponse),
    Error { message: String },
}

fn next_request_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("request-{millis}-{}-{sequence}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_response_roundtrip() {
        let request = Request::new("example.read", serde_json::json!({"id": "1"}));
        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: Request = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.operation, "example.read");

        let response = Response::success(&request.request_id, serde_json::json!({"ok": true}));
        let encoded = serde_json::to_vec(&response).unwrap();
        let decoded: Response = serde_json::from_slice(&encoded).unwrap();
        assert!(decoded.success);
        assert_eq!(decoded.request_id, request.request_id);
    }
}
