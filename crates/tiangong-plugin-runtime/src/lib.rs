//! WASM 插件运行时。
//!
//! 提供单文件 WASM Component 的加载、资源限制，以及向进程内
//! [`Plugin`](tiangong_core::core::Plugin) trait 的适配。Memory 试迁移通过通用
//! host request 连接现有 MemoryHandle；热加载、版本快照与权限探测仍未实现。

pub mod adapter;
pub mod bindings;
pub mod config;
mod execution;
pub mod host_state;
pub mod loader;
pub mod registry;

pub use adapter::WasmPluginAdapter;
pub use config::PluginRuntimeConfig;
pub use loader::{Contribution, Descriptor, Outcome, Spec, ToolCall, WasmPlugin, WasmPluginLoader};
