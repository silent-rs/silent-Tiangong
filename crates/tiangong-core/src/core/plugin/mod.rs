//! 进程内插件自注册架构（issue #156）目录化。
//!
//! 每个 [`Plugin`] 封装自己的全部能力，在 engine 创建/重建时自行向
//! [`RuntimeEngine`] 注册，消除三方中转仓库模式。
//!
//! 设计要点：
//! - [`Plugin`] 是能力的声明式聚合：通过 supertrait 约束同时要求实现工具规格、
//!   工具覆盖与 Prompt 段落三种能力（均提供默认空实现，插件按需覆写）。
//! - [`Plugin::register`] 接收 `&RuntimeEngine`（engine 内部用 `Arc` + interior
//!   mutability，`&self` 即可修改），core 在调用前会先通过 [`Plugin::set_workspace`]
//!   注入当前会话工作目录、通过 [`Plugin::set_trust_mode`] 注入共享信任模式引用、
//!   并通过 [`Plugin::set_feedback_tx`] 注入反馈通道。
//! - 能力 trait（`PageFetcher` / `TerminalProvider`）不消除，需要注入外部能力的插件
//!   仍在 [`Plugin::register`] 中调 engine 的 `set_*` 方法。
//!
//! 模块组织：
//! - [`trait_def`]：`Plugin` trait 定义与信任模式/反馈默认能力。
//! - [`feedback`]：插件状态反馈通道（`PluginFeedback` + `PluginFeedbackTx`）。
//! - [`registry`]：`register_plugin` 编排逻辑（core 在 engine 创建时遍历调用）。
//! - [`injection`]：插件事件注入通道（synthetic tool）的工具规格。
//! - [`tool_spec`]：插件基础设施相关的工具名常量集中点。
//!
//! 注意：本模块与根 `crate::plugin`（外部清单驱动插件，MCP/skill）是两套不同的机制，
//! 不要混淆。

mod feedback;
mod injection;
mod registry;
mod tool_spec;
mod trait_def;

pub use feedback::{PluginFeedback, PluginFeedbackTx};
pub use trait_def::{Plugin, check_full_trust};

pub(crate) use injection::injection_tool_spec;
pub(crate) use registry::register_plugin;
