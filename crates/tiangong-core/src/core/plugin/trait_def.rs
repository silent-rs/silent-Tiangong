//! 进程内 `Plugin` trait 定义。
//!
//! [`Plugin`] 是能力的声明式聚合：通过 supertrait 约束同时要求实现工具规格、
//! 工具覆盖与 Prompt 段落三种能力（均提供默认空实现，插件按需覆写）。
//!
//! core 通过 trait 默认方法统一注入两类运行时上下文（均在 `register` 之前调用）：
//! - [`Plugin::set_trust_mode`]：会话信任模式解析句柄（插件可据此放宽校验）。
//! - [`Plugin::set_feedback_tx`]：状态反馈通道（插件可向 session 投递外部事件）。
//!
//! 两者都带默认实现，不需要的插件无需任何改动。其中信任模式的**查询**（如
//! 各插件自定义的 `is_full_trust` 固有方法）是插件内部工具，不作为 trait 能力暴露，
//! 插件按需读取注入的解析句柄即可。

use std::path::Path;

use crate::core::plugin::feedback::PluginFeedbackTx;
use crate::permission::TrustModeHandle;
use crate::runtime::RuntimeEngine;
use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

/// 进程内插件：封装自己的全部能力，在 engine 创建/重建时自行注册。
///
/// 通过 supertrait 约束声明三种能力（`ToolSpecProvider` / `ToolOverrideHandler` /
/// `PromptSectionProvider`），三者均提供默认空实现——插件只需覆写自己关心的部分。
///
/// core 在 engine 创建时遍历插件，依次：
/// 1. [`set_workspace`](Plugin::set_workspace) 注入会话工作目录；
/// 2. [`set_trust_mode`](Plugin::set_trust_mode) 注入信任模式解析句柄；
/// 3. [`set_feedback_tx`](Plugin::set_feedback_tx) 注入状态反馈通道；
/// 4. [`register`](Plugin::register) 让插件初始化内部状态（如克隆配置）。
///
/// 工具规格 / 工具覆盖 / Prompt 段落由 core 根据 supertrait 自动收集，无需插件手动注册。
///
/// 此外，core 在 worker_loop 的关键生命周期节点遍历插件，回调下述生命周期钩子
///（均提供默认空实现），传入 `&mut Session` 供插件做必要处理（如维护索引、归档
/// 记忆等）。插件按需覆写关心的节点即可。
pub trait Plugin: ToolSpecProvider + ToolOverrideHandler + PromptSectionProvider {
    /// 插件唯一标识（日志/调试用）。
    fn id(&self) -> &str;

    /// 在 engine 创建/重建时调用，插件初始化内部状态（如克隆配置）。
    ///
    /// 工具规格、工具覆盖、Prompt 段落由 core 通过 supertrait 自动收集，无需在此手动注册。
    /// 调用此方法前，core 已通过 [`set_workspace`](Plugin::set_workspace)、
    /// [`set_trust_mode`](Plugin::set_trust_mode) 与 [`set_feedback_tx`](Plugin::set_feedback_tx)
    /// 注入会话工作目录、信任模式解析句柄与状态反馈通道，插件可在此安全读取/存储。
    fn register(&self, _engine: &RuntimeEngine) {}

    /// 注入当前会话的工作目录。
    ///
    /// core 在 engine 创建时以及每次会话工作目录变更（`Command::UpdateCwd`）时调用。
    /// 传入 `None` 表示当前会话无有效工作目录（如 cwd 为空或不存在），插件应清空
    /// 之前缓存的 workspace，避免在旧目录上继续操作。
    ///
    /// 默认实现为空操作；需要感知工作目录的插件（如文件工具）应覆写此方法，将路径
    /// 存入内部状态，供后续工具调用使用。
    fn set_workspace(&self, _workspace: Option<&Path>) {}

    /// 注入信任模式解析句柄（与 [`crate::permission::PermissionGate`] 共享同一解析器）。
    ///
    /// core 在 engine 创建时于 [`Plugin::register`] 之前调用一次。需要感知信任模式的
    /// 插件（如 `fs` / `command` / `fetch`）应覆写此方法，把入参存入内部字段，
    /// 之后在其自身的固有方法里读取（例如 `is_full_trust`）。
    ///
    /// 默认实现为空操作——不关心信任模式的插件（`scheduler` / `terminal` / `browser`）
    /// 无需覆写；这些插件的工具执行仍受 engine 层 [`PermissionGate::check`] 统一兜底。
    ///
    /// 注意：信任模式的查询是**插件内部工具**，不作为 `Plugin` trait 的状态/能力暴露。
    /// 插件调用句柄的 `current()` 读取当前会话的有效模式。
    fn set_trust_mode(&self, _trust: TrustModeHandle) {}

    /// 注入状态反馈通道（复用 worker 的命令通道）。
    ///
    /// core 在 engine 创建时于 [`Plugin::register`] 之前调用一次。需要向 session 主动
    /// 投递外部事件（如浏览器页面变化、终端用户操作）的插件应覆写此方法，把入参
    /// clone 后存入内部字段，之后通过 [`PluginFeedbackTx::send`] 投递
    /// [`PluginFeedback`]，core 会统一注入到 session（以 tool result 形式）。
    ///
    /// 默认实现为空操作——不需要主动投递外部事件的插件无需覆写。
    fn set_feedback_tx(&self, _tx: PluginFeedbackTx) {}
    /// 收集插件贡献的子进程环境变量（供 run_command 等子进程执行注入）。
    ///
    /// core 在「所有插件注册完成时」以及「配置变化导致 engine rebuild 时」统一
    /// 遍历所有插件调用此方法，合并结果写入 RuntimeEngine 的 runtime_env，
    /// 供 command 插件在执行子进程时注入。
    ///
    /// 默认返回空——不贡献环境变量的插件（fs / fetch / memory / scheduler 等）
    /// 无需覆写。需要贡献 env 的插件（如注入外部服务 env、读取 .env.local 等）
    /// 覆写此方法返回自己的环境变量。
    fn collect_exec_env(&self) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::new()
    }

    /// 贡献插件工具的权限等级覆盖（供 core 权限门统一汇总）。
    ///
    /// core 在「所有插件注册完成时」统一遍历所有插件调用此方法，合并到
    /// `PermissionGate` 的覆盖表——避免 core 的 `classify_tool` 硬编码任何插件
    /// 工具名。插件工具名未命中覆盖表时走 core 的默认分类（按工具名前缀/特征推断）。
    ///
    /// 默认返回空——不贡献权限覆盖的插件无需覆写。
    fn tool_permission_overrides(
        &self,
    ) -> std::collections::BTreeMap<String, crate::permission::PermissionLevel> {
        std::collections::BTreeMap::new()
    }

    /// 用户取消当前 turn 时通知插件响应取消意图。
    ///
    /// Core 在判定 turn 终态为 `Cancelled` 后、`on_turn_finished` 之前调用。
    /// 插件可在此中断与当前 turn 关联的副作用（如取消子 Agent 执行、暂停页面
    /// 观察），但**不应销毁自身状态**——Core 仍会存活，用户可能继续发消息。
    ///
    /// 与 `on_session_ended` 的区别：
    /// - `on_cancel`：turn 被取消，Core 继续存活，插件保留状态。
    /// - `on_session_ended`：Core 即将退出，插件做最终清理。
    ///
    /// 默认实现为空——不关心取消的插件无需覆写。
    fn on_cancel<'a>(
        &'a self,
        _session: &mut crate::session::Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    // ── 配置与生命周期钩子 ──

    /// Core 配置快照更新后调用。
    ///
    /// worker_loop 在首次 build engine 以及 config generation 变化导致 engine rebuild 时
    /// 调用（在 [`Plugin::register`] 之后、[`Plugin::on_engine_rebuilt`] 之前）。插件可
    /// 按需读取模型配置、memory 配置等，执行热更新（如 reconfigure memory actor）。
    /// 默认实现为空。
    fn on_config_updated(&self, _config: &crate::core_config::CoreConfig) {}

    // ── 生命周期钩子 ──
    //
    // 在 worker_loop 的对应节点遍历插件回调，传入 `&mut Session` 供插件处理
    //（如维护索引、归档记忆等）。全部默认空实现，插件按需覆写。

    /// worker 首次处理命令前会按需 build engine；首次 build + 插件注册完成后调用一次。
    ///
    /// 注意：此钩子在「收到首条命令后、处理该命令前」触发（worker 收到命令才会按需
    /// build engine），并非在接收命令前。此时 [`set_workspace`](Plugin::set_workspace) /
    /// [`set_trust_mode`](Plugin::set_trust_mode) / [`set_feedback_tx`](Plugin::set_feedback_tx)
    /// 均已注入，插件可安全读取已存储的上下文。适合做一次性的会话级初始化（如对工作
    /// 目录做首次全量扫描）。仅触发一次；后续 engine 重建只回调 [`Plugin::on_engine_rebuilt`]。
    fn on_session_ready(&self, _session: &mut crate::session::Session) {}

    /// engine 创建或重建（配置变更）完成后调用。
    fn on_engine_rebuilt(&self, _session: &mut crate::session::Session) {}

    /// 会话工作目录变更后调用（core 已更新 `session.cwd` 并重注入 `set_workspace`）。
    fn on_cwd_changed(&self, _session: &mut crate::session::Session) {}

    /// 一个对话轮次开始前调用：用户消息已写入 session，`execute_turn` 调用前。
    ///
    /// `turn_start_idx` 为本轮用户消息在 `session.messages` 中的起始索引，
    /// 可供 [`Plugin::on_turn_finished`] 计算本轮新增消息范围。
    fn on_turn_started(&self, _session: &mut crate::session::Session, _turn_start_idx: usize) {}

    /// 一个对话轮次结束后调用：执行时长与 turn_result 已写入并落盘。
    ///
    /// `turn_start_idx` 与 [`Plugin::on_turn_started`] 接收的值一致，可用于取出本轮
    /// 新增的消息做后处理（如批量写入索引）。
    fn on_turn_finished(&self, _session: &mut crate::session::Session, _turn_start_idx: usize) {}

    /// 会话结束、worker 即将退出前调用（finalize 用）。
    ///
    /// 注意：此时 stream 通道可能已关闭，钩子内不应再投递流事件。
    fn on_session_ended(&self, _session: &mut crate::session::Session) {}
}
