//! IPC 帧协议定义（TCP loopback + JSON Lines）

use serde::{Deserialize, Serialize};

/// Endpoint 发现信息，写入本地 runtime 文件供 follower 读取。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcEndpoint {
    pub service: String,
    pub host: String,
    pub port: u16,
    pub pid: u32,
    pub token: String,
    pub updated_at: String,
}

/// IPC 请求帧
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub request_id: String,
    pub payload: serde_json::Value,
}

/// IPC 响应帧
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub request_id: String,
    pub payload: serde_json::Value,
}

/// 连接建立后的第一帧，使用 token 做本地鉴权。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcAuth {
    pub token: String,
}

/// JSON Lines 传输帧
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcFrame {
    Auth(IpcAuth),
    Request(IpcRequest),
    Response(IpcResponse),
    Error { message: String },
}
