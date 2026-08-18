//! Command 插件私有业务协议。
//!
//! 本 crate 只定义 WASM 与 sidecar 共同使用的操作、请求和响应，不包含 IPC、
//! 进程、子进程、文件系统或 Wasmtime 依赖。可同时编译为本机与 `wasm32-wasip2`。
//!
//! 沙箱预留：`CommandAccessContext` 把"当前会话的访问能力"显式建模成请求字段
//! （当前只有 `full_trust` + `workspace` + `allowed_commands`），未来细化权限
//! （如禁用命令列表、网络出口白名单、env 黑名单）只需扩展该结构并 bump
//! business-protocol，不动 WIT/wasm 桥接。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub mod exec;

pub const PLUGIN_ID: &str = "command";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COMMAND_PROTOCOL_VERSION: u32 = 1;

/// 工具名常量（与工具规格、handle_tool 路由对齐）。
pub const TOOL_RUN_COMMAND: &str = "run_command";
pub const TOOL_RUN_SHELL: &str = "run_shell";

/// 一个类型化 Command 业务操作。
///
/// 每个操作由零字段 marker struct 实现，提供操作名常量与关联的请求/响应类型。
/// WASM 端通过 `sidecar_client::invoke::<O>()` 泛型调用，以 `NAME` 作为 operation、
/// 序列化 `Request`、反序列化 `Response`。
pub trait CommandOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ack {}

/// 当前会话的命令访问能力（沙箱预留点 B）。
///
/// 当前承载 `full_trust` / `workspace` / `allowed_commands`；未来引入沙箱
/// （命令 AST 校验、env 黑名单、网络出口控制等）时，扩展本结构并 bump
/// business-protocol 即可，WIT/wasm 不动。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandAccessContext {
    /// 当前会话工作目录（cwd 解析的 base）。
    #[serde(default)]
    pub workspace: Option<String>,
    /// 是否完全信任模式（跳过命令/路径校验，与原进程内实现一致）。
    #[serde(default)]
    pub full_trust: bool,
    /// 用户自定义允许命令列表（扩展内置白名单）。
    #[serde(default)]
    pub allowed_commands: Vec<String>,
}
