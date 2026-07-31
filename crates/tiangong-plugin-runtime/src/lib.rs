//! WASM 插件运行时。
//!
//! 提供单文件 WASM Component 的加载、资源限制，以及向进程内
//! [`Plugin`](tiangong_core::core::Plugin) trait 的适配。
//! 通用运行时不依赖任何具体插件（如 Memory），业务逻辑由入口侧注入。

pub mod adapter;
pub mod bindings;
pub mod config;
mod execution;
pub mod host_state;
pub mod loader;
pub mod registry;
pub mod sidecar;

pub use adapter::WasmPluginAdapter;
pub use config::PluginRuntimeConfig;
pub use loader::{Contribution, Descriptor, Outcome, Spec, ToolCall, WasmPlugin, WasmPluginLoader};
pub use sidecar::SidecarConnection;
