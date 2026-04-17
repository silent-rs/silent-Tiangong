//! IPC 帧协议定义

use serde::{Deserialize, Serialize};

/// IPC 请求帧
#[derive(Debug, Serialize, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IpcRequest {
    pub(crate) request_id: String,
    pub(crate) payload: serde_json::Value,
}

/// IPC 响应帧
#[derive(Debug, Serialize, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IpcResponse {
    pub(crate) request_id: String,
    pub(crate) payload: serde_json::Value,
}
