//! Memory 插件私有业务协议。
//!
//! 本 crate 只定义 WASM 与 sidecar 共同使用的操作、请求和响应，不包含 IPC、
//! 进程、文件系统、模型或 Wasmtime 依赖。

use serde::Serialize;
use serde::de::DeserializeOwned;

pub mod control;
pub mod error;
pub mod injection;
pub mod recall;
pub mod rumination;
pub mod ui;

pub const PLUGIN_ID: &str = "memory";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MEMORY_PROTOCOL_VERSION: u32 = 1;

/// 一个类型化 Memory 业务操作。
pub trait MemoryOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct Empty {}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct Ack {
    #[serde(default)]
    pub kind: Option<String>,
}
