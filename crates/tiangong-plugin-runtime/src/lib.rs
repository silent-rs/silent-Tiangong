//! WASM 插件运行时。
//!
//! 阶段一 PoC：提供单文件 WASM Component 的加载、资源限制（fuel + epoch + 内存）
//! 与向进程内 [`Plugin`](tiangong_core::core::Plugin) trait 的适配。
//!
//! 见 issue #321 / #301。当前不接入任何 host import，不实现热加载、版本快照
//! 与权限探测；示例 memory 插件以纯 mock 数据验证调用链路。

pub mod adapter;
pub mod bindings;
pub mod config;
pub mod host_state;
pub mod loader;
pub mod registry;

pub use adapter::WasmPluginAdapter;
pub use config::PluginRuntimeConfig;
pub use loader::{
    Descriptor, FusedHit, MemoryKind, Outcome, PlannedRecall, SearchStrategy, Spec, ToolCall,
    WasmPlugin, WasmPluginLoader,
};
