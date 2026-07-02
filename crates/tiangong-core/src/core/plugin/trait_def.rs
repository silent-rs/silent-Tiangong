//! 进程内 `Plugin` trait 定义。
//!
//! [`Plugin`] 是能力的声明式聚合：通过 supertrait 约束同时要求实现工具规格、
//! 工具覆盖与 Prompt 段落三种能力（均提供默认空实现，插件按需覆写）。
//!
//! core 通过 trait 默认方法统一注入两类运行时上下文（均在 `register` 之前调用）：
//! - [`Plugin::set_trust_mode`]：共享信任模式引用（插件可据此放宽校验）。
//! - [`Plugin::set_feedback_tx`]：状态反馈通道（插件可向 session 投递外部事件）。
//!
//! 两者都带默认实现，不需要的插件无需任何改动。其中信任模式的**查询**（如
//! `is_full_trust`）是插件内部工具，不作为 trait 能力暴露，插件借助
//! [`check_full_trust`] 复用样板即可。

use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::core::plugin::feedback::PluginFeedbackTx;
use crate::permission::TrustMode;
use crate::runtime::RuntimeEngine;
use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

/// 进程内插件：封装自己的全部能力，在 engine 创建/重建时自行注册。
///
/// 通过 supertrait 约束声明三种能力（`ToolSpecProvider` / `ToolOverrideHandler` /
/// `PromptSectionProvider`），三者均提供默认空实现——插件只需覆写自己关心的部分。
///
/// core 在 engine 创建时遍历插件，依次：
/// 1. [`set_workspace`](Plugin::set_workspace) 注入会话工作目录；
/// 2. [`set_trust_mode`](Plugin::set_trust_mode) 注入共享信任模式引用；
/// 3. [`set_feedback_tx`](Plugin::set_feedback_tx) 注入状态反馈通道；
/// 4. [`register`](Plugin::register) 让插件注入外部能力（如 PageFetcher）。
///
/// 工具规格 / 工具覆盖 / Prompt 段落由 core 根据 supertrait 自动收集，无需插件手动注册。
pub trait Plugin: ToolSpecProvider + ToolOverrideHandler + PromptSectionProvider {
    /// 插件唯一标识（日志/调试用）。
    fn id(&self) -> &str;

    /// 在 engine 创建/重建时调用，插件注入外部能力（如 PageFetcher / TerminalProvider）。
    ///
    /// 工具规格、工具覆盖、Prompt 段落由 core 通过 supertrait 自动收集，无需在此手动注册。
    /// 调用此方法前，core 已通过 [`set_workspace`](Plugin::set_workspace)、
    /// [`set_trust_mode`](Plugin::set_trust_mode) 与 [`set_feedback_tx`](Plugin::set_feedback_tx)
    /// 注入会话工作目录、共享信任模式引用与状态反馈通道，插件可在此安全读取/存储。
    fn register(&self, _engine: &RuntimeEngine) {}

    /// 注入当前会话的工作目录。
    ///
    /// core 在 engine 创建时以及每次会话工作目录变更（`Command::UpdateCwd`）时调用。
    /// 默认实现为空操作；需要感知工作目录的插件（如文件工具）应覆写此方法，将路径
    /// 存入内部状态，供后续工具调用使用。
    fn set_workspace(&self, _workspace: &Path) {}

    /// 注入共享信任模式引用（与 [`crate::permission::PermissionGate`] 共享同一个 `RwLock`）。
    ///
    /// core 在 engine 创建时于 [`Plugin::register`] 之前调用一次。需要感知信任模式的
    /// 插件（如 `fs` / `command` / `fetch`）应覆写此方法，把入参存入内部字段，
    /// 之后在其自身的固有方法里读取（例如 `is_full_trust`）。
    ///
    /// 默认实现为空操作——不关心信任模式的插件（`scheduler` / `terminal` / `browser`）
    /// 无需覆写；这些插件的工具执行仍受 engine 层 [`PermissionGate::check`] 统一兜底。
    ///
    /// 注意：信任模式的查询是**插件内部工具**，不作为 `Plugin` trait 的状态/能力暴露。
    /// 插件可借助 [`check_full_trust`] 复用「读 RwLock → 判等」样板。
    fn set_trust_mode(&self, _trust: Arc<RwLock<TrustMode>>) {}

    /// 注入状态反馈通道（复用 worker 的命令通道）。
    ///
    /// core 在 engine 创建时于 [`Plugin::register`] 之前调用一次。需要向 session 主动
    /// 投递外部事件（如浏览器页面变化、终端用户操作）的插件应覆写此方法，把入参
    /// clone 后存入内部字段，之后通过 [`PluginFeedbackTx::send`] 投递
    /// [`PluginFeedback`]，core 会统一注入到 session（以 tool result 形式）。
    ///
    /// 默认实现为空操作——不需要主动投递外部事件的插件无需覆写。
    fn set_feedback_tx(&self, _tx: PluginFeedbackTx) {}
}

/// 读取共享信任模式引用并判断是否为 [`TrustMode::FullTrust`]。
///
/// 供插件在自身的「是否完全信任」查询（如 `is_full_trust` 固有方法）中复用，
/// 消除重复的「读 RwLock → 判等」样板。入参为 `Option` 引用：`None` 或读取锁失败
/// 均返回 `false`（安全降级）。
///
/// # Examples
///
/// ```
/// # use std::sync::{Arc, RwLock};
/// # use tiangong_core::permission::TrustMode;
/// # use tiangong_core::core::plugin::check_full_trust;
/// let cell: Arc<RwLock<TrustMode>> = Arc::new(RwLock::new(TrustMode::FullTrust));
/// assert!(check_full_trust(Some(&cell)));
/// assert!(!check_full_trust(None));
/// ```
pub fn check_full_trust(trust: Option<&Arc<RwLock<TrustMode>>>) -> bool {
    let Some(handle) = trust else {
        return false;
    };
    handle
        .read()
        .map(|g| *g == TrustMode::FullTrust)
        .unwrap_or(false)
}
