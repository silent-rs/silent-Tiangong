//! 进程内 `Plugin` trait 定义。
//!
//! [`Plugin`] 是能力的声明式聚合：通过 supertrait 约束同时要求实现工具规格、
//! 工具覆盖与 Prompt 段落三种能力（均提供默认空实现，插件按需覆写）。
//!
//! core 通过 trait 默认方法统一注入运行时上下文：
//! - [`Plugin::set_workspace`]：会话工作目录（文件类工具据此感知 cwd）。
//! - [`Plugin::set_trust_mode`]：会话信任模式（插件可据此放宽校验）。
//! - [`Plugin::set_feedback_tx`]：状态反馈通道（插件可向 session 投递外部事件）。
//!
//! workspace 与 trust mode 在收集 tool_specs 前注入；feedback 在 TurnContext 构建后、
//! `on_session_ready` 与 turn task 启动前注入。三者都带默认实现，不需要的插件无需
//! 任何改动。其中信任模式的**查询**（如各插件自定义的 `is_full_trust` 固有方法）
//! 是插件内部工具，不作为 trait 能力暴露，插件按需读取注入的信任模式即可。

use std::path::Path;

use crate::core::plugin::feedback::PluginFeedbackTx;
use crate::permission::TrustMode;
use crate::tool_override::{
    MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
};

/// 进程内插件：封装自己的全部能力，在 engine 创建/重建时自行注册。
///
/// 通过 supertrait 约束声明三种能力（`ToolSpecProvider` / `ToolOverrideHandler` /
/// `PromptSectionProvider`），三者均提供默认空实现——插件只需覆写自己关心的部分。
///
/// core 在每轮 Context 准备与启动时遍历插件，依次：
/// 1. [`set_workspace`](Plugin::set_workspace) 注入会话工作目录；
/// 2. [`set_trust_mode`](Plugin::set_trust_mode) 注入信任模式；
/// 3. 收集 `tool_specs` / 注册 override handler 并构建 TurnContext；
/// 4. [`set_feedback_tx`](Plugin::set_feedback_tx) 注入本轮状态反馈通道；
/// 5. 首轮调用 `on_session_ready`，然后收集 Prompt 段落写入 Session。
///
/// 插件初始化自身状态（如读取配置、启动后台调度器）在 [`Plugin::on_config_updated`]
/// 中完成（core 在收集 specs 前调用）。
/// 工具规格 / 工具覆盖 / Prompt 段落由 core 根据 supertrait 自动收集，无需插件手动注册。
///
/// 此外，core 在 worker_loop 的关键生命周期节点遍历插件，回调下述生命周期钩子
///（均提供默认空实现），传入 `&mut Session` 供插件做必要处理（如维护索引、归档
/// 记忆等）。插件按需覆写关心的节点即可。
pub trait Plugin:
    ToolSpecProvider + ToolOverrideHandler + PromptSectionProvider + MentionCandidateProvider
{
    /// 插件唯一标识（日志/调试用）。
    fn id(&self) -> &str;

    /// 注入当前会话的工作目录。
    ///
    /// core 在 engine 创建时（每 turn 现建）调用，工作目录变更在下次 turn 开始时
    /// 自动生效。传入 `None` 表示当前会话无有效工作目录（如 cwd 为空或不存在），
    /// 插件应清空之前缓存的 workspace，避免在旧目录上继续操作。
    ///
    /// 默认实现为空操作；需要感知工作目录的插件（如文件工具）应覆写此方法，将路径
    /// 存入内部状态，供后续工具调用使用。工作区变更后的副作用（如重建索引）也应
    /// 在此方法内触发，避免职责分散到多个钩子。
    fn set_workspace(&self, _workspace: Option<&Path>) {}

    /// 注入当前会话的信任模式（[`crate::permission::TrustMode`]）。
    ///
    /// core 在 engine 创建时、收集 tool_specs 之前调用一次。需要感知信任模式的
    /// 插件（如 `fs` / `command` / `fetch`）应覆写此方法，把入参存入内部字段，
    /// 之后在其自身的固有方法里读取（例如 `is_full_trust`）。
    ///
    /// 默认实现为空操作——不关心信任模式的插件（`scheduler` / `terminal` / `browser`）
    /// 无需覆写；这些插件的工具执行仍受 engine 层信任模式审批统一兜底
    ///（`FullTrust` 放行一切，否则走 turn 层审批流程）。
    ///
    /// 注意：信任模式的查询是**插件内部工具**，不作为 `Plugin` trait 的状态/能力暴露。
    fn set_trust_mode(&self, _trust: TrustMode) {}

    /// 注入状态反馈通道（复用 worker 的命令通道）。
    ///
    /// core 在 TurnContext 构建后、turn task 启动前调用一次。需要向 session 主动
    /// 投递外部事件（如浏览器页面变化、终端用户操作）的插件应覆写此方法，把入参
    /// clone 后存入内部字段，之后通过 [`PluginFeedbackTx`] 投递会话内容、用量或
    /// 流事件，Core 会按本轮命令顺序统一处理。
    ///
    /// 默认实现为空操作——不需要主动投递外部事件的插件无需覆写。
    fn set_feedback_tx(&self, _tx: PluginFeedbackTx) {}

    /// 贡献插件自身的子进程环境变量。
    ///
    /// core 在「所有插件注册完成时」以及「配置变化导致 engine rebuild 时」统一
    /// 遍历所有插件调用此方法，合并全部贡献后通过 [`Plugin::set_exec_env`] 回注。
    ///
    /// 默认返回空——不贡献环境变量的插件（fs / fetch / memory / scheduler 等）
    /// 无需覆写。需要贡献 env 的插件（如注入外部服务 env、读取 .env.local 等）
    /// 覆写此方法返回自己的环境变量。
    fn exec_env(&self) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::new()
    }

    /// 回注全部插件汇总后的子进程环境变量。
    ///
    /// core 在汇总全部插件的 [`exec_env`](Plugin::exec_env) 后遍历所有插件调用此方法，
    /// 传入合并后的完整 env。需要读取合并 env 的插件（如 `command` / `task`，在
    /// 执行子进程时注入）应覆写此方法存储。默认空操作——不消费 env 的插件无需覆写。
    fn set_exec_env(&self, _env: std::collections::BTreeMap<String, String>) {}

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
    /// 调用（在收集 tool_specs 之前）。插件可按需读取模型配置、memory 配置等，执行
    /// 热更新或初始化（如 reconfigure memory actor、启动后台调度器）。默认实现为空。
    fn on_config_updated(&self, _config: &crate::core_config::CoreConfig) {}

    // ── 生命周期钩子 ──
    //
    // 在 worker_loop 的对应节点遍历插件回调，传入 `&mut Session` 供插件处理
    //（如维护索引、归档记忆等）。全部默认空实现，插件按需覆写。

    /// 首轮 TurnContext 构建且 feedback 注入完成后、turn task 启动前调用一次。
    ///
    /// 此时 [`set_workspace`](Plugin::set_workspace) /
    /// [`set_trust_mode`](Plugin::set_trust_mode) / [`set_feedback_tx`](Plugin::set_feedback_tx)
    /// 均已注入，插件可安全读取已存储的上下文。适合做一次性的会话级初始化（如对工作
    /// 目录做首次全量扫描）。仅触发一次；后续 Context 重建由 [`Plugin::on_config_updated`]
    /// 承载再配置语义。
    fn on_session_ready(&self, _session: &mut crate::session::Session) {}

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
