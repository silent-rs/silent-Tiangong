//! WASM 插件运行时。
//!
//! 提供单文件 WASM Component 的加载、资源限制、宿主适配，以及配套
//! sidecar 的通用协议和运行管理。

pub mod adapter;
pub mod bindings;
pub mod config;
mod execution;
pub mod host_state;
pub mod loader;
pub mod manifest;
pub mod protocol;
pub mod registry;
pub mod sidecar;

pub use adapter::WasmPluginAdapter;
pub use config::PluginRuntimeConfig;
pub use loader::{Contribution, Descriptor, Outcome, Spec, ToolCall, WasmPlugin, WasmPluginLoader};
pub use sidecar::{ProcessSidecarConnection, SidecarConfig, SidecarConnection};
