//! WASM 插件运行时。
//!
//! 提供单文件 WASM Component 的加载、资源限制、宿主适配，以及配套
//! sidecar 的通用协议和运行管理。
//!
//! # 公共中立约束
//!
//! 本 crate 必须始终保持插件无关，不得包含任何具体插件的 ID、操作名、协议类型、
//! 数据路径、业务事件或条件分支。单个插件需要的行为必须放在该插件的 WASM、私有
//! 协议或 sidecar 中。只有能被任意插件复用、且无需理解业务负载的机制，才能进入
//! 运行时。完整约束见 `docs/plugin-development.md` 的“WASM Runtime 公共中立约束”。

pub mod adapter;
pub mod artifacts;
pub mod bindings;
pub mod bridge;
pub mod config;
mod execution;
pub mod host_state;
pub mod loader;
pub mod manifest;
pub mod protocol;
pub mod registry;
pub mod seams;
pub mod sidecar;
pub mod signature;
pub mod slots;

pub use adapter::WasmPluginAdapter;
pub use bridge::{
    BRIDGE_NAMESPACES, EVENT_NAMESPACE_PREFIXES, bridge_call, bridge_emit, bridge_subscribe,
    bridge_unsubscribe, set_event_emitter,
};
pub use config::PluginRuntimeConfig;
pub use loader::{
    Contribution, Descriptor, MentionCandidate, Outcome, Spec, ToolCall, WasmPlugin,
    WasmPluginLoader,
};
pub use seams::{SeamHub, SeamKind, SeamRegistration};
pub use sidecar::{ProcessSidecarConnection, SidecarConfig, SidecarConnection};
pub use slots::{
    BUILTIN_SLOTS, OPEN_MODE_SLOT, OpenMode, SandboxKind, SlotContextKey, SlotDescriptor,
    SlotInstances, SlotRegistry, UiContribution,
};
