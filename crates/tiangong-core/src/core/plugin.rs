//! 进程内插件自注册架构（issue #156）。
//!
//! 每个 [`Plugin`] 封装自己的全部能力，在 engine 创建/重建时自行向
//! [`RuntimeEngine`] 注册，消除三方中转仓库模式。
//!
//! 设计要点：
//! - [`Plugin`] 是能力的声明式聚合：通过 supertrait 约束同时要求实现工具规格、
//!   工具覆盖与 Prompt 段落三种能力（均提供默认空实现，插件按需覆写）。
//! - [`Plugin::register`] 接收 `&RuntimeEngine`（engine 内部用 `Arc` + interior
//!   mutability，`&self` 即可修改），core 在调用前会先通过 [`Plugin::set_workspace`]
//!   注入当前会话工作目录。
//! - 能力 trait（`PageFetcher` / `TerminalProvider`）不消除，需要注入外部能力的插件
//!   仍在 [`Plugin::register`] 中调 engine 的 `set_*` 方法。
//!
//! 注意：本模块与根 `crate::plugin`（外部清单驱动插件，MCP/skill）是两套不同的机制，
//! 不要混淆。

use std::path::Path;

use crate::runtime::RuntimeEngine;
use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

/// 进程内插件：封装自己的全部能力，在 engine 创建/重建时自行注册。
///
/// 通过 supertrait 约束声明三种能力（`ToolSpecProvider` / `ToolOverrideHandler` /
/// `PromptSectionProvider`），三者均提供默认空实现——插件只需覆写自己关心的部分。
/// core 在 engine 创建时遍历插件，先 [`set_workspace`](Plugin::set_workspace) 注入会话
/// 工作目录，再 [`register`](Plugin::register) 让插件注入外部能力（如 PageFetcher）；
/// 工具规格 / 工具覆盖 / Prompt 段落由 core 根据 supertrait 自动收集，无需插件手动注册。
pub trait Plugin: ToolSpecProvider + ToolOverrideHandler + PromptSectionProvider {
    /// 插件唯一标识（日志/调试用）。
    fn id(&self) -> &str;

    /// 在 engine 创建/重建时调用，插件注入外部能力（如 PageFetcher / TerminalProvider）。
    ///
    /// 工具规格、工具覆盖、Prompt 段落由 core 通过 supertrait 自动收集，无需在此手动注册。
    fn register(&self, _engine: &RuntimeEngine) {}

    /// 注入当前会话的工作目录。
    ///
    /// core 在 engine 创建时以及每次会话工作目录变更（`Command::UpdateCwd`）时调用。
    /// 默认实现为空操作；需要感知工作目录的插件（如文件工具）应覆写此方法，将路径
    /// 存入内部状态，供后续工具调用使用。
    fn set_workspace(&self, _workspace: &Path) {}
}
