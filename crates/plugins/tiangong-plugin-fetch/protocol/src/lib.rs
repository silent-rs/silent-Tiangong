//! Fetch 插件私有业务协议。
//!
//! 本 crate 只定义 WASM 与 sidecar 共同使用的操作、请求和响应，不包含 IPC、
//! 进程、网络、文件系统或 Wasmtime 依赖。可同时编译为本机与 `wasm32-wasip2`。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub mod web_fetch;

pub const PLUGIN_ID: &str = "fetch";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const FETCH_PROTOCOL_VERSION: u32 = 1;

/// 工具名常量（与工具规格、handle_tool 路由对齐）。
pub const TOOL_WEB_FETCH: &str = "web_fetch";

/// 一个类型化 Fetch 业务操作。
///
/// 每个操作由零字段 marker struct 实现，提供操作名常量与关联的请求/响应类型。
/// WASM 端通过 `sidecar_client::invoke::<O>()` 泛型调用，以 `NAME` 作为 operation、
/// 序列化 `Request`、反序列化 `Response`。
pub trait FetchOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ack {}

/// web_fetch 执行模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchMode {
    #[default]
    Text,
    Download,
}

/// text 模式正文提取方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractMode {
    #[default]
    Auto,
    Text,
    Raw,
}
