//! 插件 → core 的语义反馈通道。
//!
//! 让插件向 core 投递**语义事件**，由 core 统一决定如何处理。插件只描述“发生了
//! 什么”，core 决策“如何处理”（累加 usage / 注入 session / 转发流事件）。
//!
//! 两类投递，链路不同：
//!
//! - **会话注入**（[`PluginFeedbackTx::inject_tool`]）：走 worker 命令队列
//!  （[`Command::InjectTool`]），由 agent loop drain 时注入 session。适合不要求
//!  即时性的外部事件（浏览器页面变化、终端用户操作）。
//!
//! - **用量上报**（[`PluginFeedbackTx::report_token_usage`]）：走 **core 拥有的
//!   turn-scoped usage sink**，即时累加到本轮用量并立即发送 `StreamEvent::TokenUsage`，
//!   **不经过命令队列**。这样插件工具完成 multimodal 子调用时 usage 立即落账，
//!   不依赖 agent loop 何时 drain 命令队列，也不会被 `check_cancel` 等只想检查
//!   取消的 drain 路径吞掉（见 [`TurnUsageSink`] 的作用域说明）。

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use crate::core::command::Command;

// ── 会话注入反馈（走命令队列）──────────────────────────────────

/// 插件向 core 投递的会话注入反馈（走命令队列）。
///
/// 与 [`Command::InjectTool`] 同构：`tool_name` + 结构化 `payload`。core 的 worker
/// 把它注入到 session，以 tool result 形式出现在对话中。`payload` 返回 JSON 而非
/// 文本，让 worker 侧根据 `tool_name` 决定呈现格式，同时保留结构化数据供去重等
/// 逻辑使用（与 [`crate::agent_input::ToolInput`] 协议一致）。
#[derive(Debug, Clone)]
pub struct PluginFeedback {
    /// 工具名（伪造 tool_call 的 name 字段，如 `plugin_injection`）。
    pub tool_name: String,
    /// 注入到对话的结构化内容（JSON）。
    pub payload: serde_json::Value,
}

impl PluginFeedback {
    /// 便捷构造。
    pub fn new(tool_name: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            payload,
        }
    }
}

// ── Turn-scoped usage sink（即时落账，不走命令队列）──────────────

/// 本轮 turn 的 usage 收集绑定。
///
/// 由 `execute_turn` 在 turn 开始时创建：`usage` 是本轮专用的累加器（turn 结束时
/// 折算进 `accumulated_usage`），`stream_tx` 用于即时发送 `StreamEvent::TokenUsage`。
/// 装进 [`TurnUsageSink`] 的可重绑定插槽里，供插件即时调用。
struct TurnUsageBinding {
    /// 本轮专用的插件 usage 累加器（turn 结束时并入 accumulated_usage）。
    usage: Mutex<tiangong_types::TokenUsage>,
    /// 即时发送 TokenUsage 流事件。
    stream_tx: std::sync::mpsc::Sender<tiangong_types::StreamEvent>,
    /// 上下文上限（填充 StreamEvent::TokenUsage.context_limit_tokens）。
    context_limit: usize,
}

/// Turn-scoped 插件 usage 收集器。
///
/// 一个可重绑定的共享插槽：turn 开始时 core 绑定本轮的 [`TurnUsageBinding`]，turn
/// 结束时解绑（清空）。插件通过 [`PluginFeedbackTx`] 持有同一个 `Arc<TurnUsageSink>`
/// 引用，调用 [`TurnUsageSink::report`] 时即时累加并发送——**不经过命令队列**，
/// 因此不受 agent loop drain 时机影响，也会被 `check_cancel` 等 drain 吞掉。
///
/// 作用域保证：turn 结束后立即解绑，迟到的 usage（如上一轮后台任务迟到上报）会被
/// 静默丢弃并打 debug 日志，不会错误计入下一轮。
///
/// # 当前边界：单活跃 turn
///
/// 当前 `TurnUsageSink` 按单活跃 turn 设计（单槽 `Option<TurnUsageBinding>`）。主
/// Agent 执行中若嵌套 Sub Agent 且 Sub Agent 也 `bind()`，会覆盖主 Agent 的 binding；
/// Sub Agent 结束时 guard drop 又会清空 binding，导致主 Agent 后续插件 usage 被丢弃。
/// 当前 Sub Agent 尚未插件化，不构成阻塞；后续 Sub Agent 插件化改造时需要改为
/// 栈式 binding 或 agent-scoped binding。
#[derive(Clone)]
pub struct TurnUsageSink {
    binding: Arc<Mutex<Option<TurnUsageBinding>>>,
}

impl TurnUsageSink {
    /// 构造空的 sink（无 turn 绑定）。
    pub fn new() -> Self {
        Self {
            binding: Arc::new(Mutex::new(None)),
        }
    }

    /// 绑定本轮 turn 的 usage 收集上下文。
    ///
    /// 在 `execute_turn` 开始时调用：传入 stream 发送端与 context_limit。返回的
    /// [`TurnUsageGuard`] 在 drop 时自动解绑，保证 turn 结束后迟到的 usage 不会计入
    /// 下一轮。本轮累计的 usage 通过 [`take_usage`](Self::take_usage) 在 turn 结束前取出。
    pub fn bind(
        &self,
        stream_tx: std::sync::mpsc::Sender<tiangong_types::StreamEvent>,
        context_limit: usize,
    ) -> TurnUsageGuard {
        if let Ok(mut guard) = self.binding.lock() {
            *guard = Some(TurnUsageBinding {
                usage: Mutex::new(tiangong_types::TokenUsage::default()),
                stream_tx,
                context_limit,
            });
        }
        TurnUsageGuard {
            sink: Arc::clone(&self.binding),
        }
    }

    /// 取出本轮累计的插件 usage（turn 结束时调用，并入 accumulated_usage）。
    ///
    /// 取出后清空本轮累加器（但不解绑绑定，解绑由 [`TurnUsageGuard`] drop 负责）。
    /// 无绑定或无累计时返回 `TokenUsage::default()`。
    pub fn take_usage(&self) -> tiangong_types::TokenUsage {
        let Ok(guard) = self.binding.lock() else {
            return tiangong_types::TokenUsage::default();
        };
        let Some(binding) = guard.as_ref() else {
            return tiangong_types::TokenUsage::default();
        };
        binding
            .usage
            .lock()
            .map(|mut u| std::mem::take(&mut *u))
            .unwrap_or_default()
    }

    /// 上报一笔插件内部产生的 LLM token 用量。
    ///
    /// 命中本轮绑定时：立即累加到本轮专用累加器，并立即发送 `StreamEvent::TokenUsage`
    /// （对齐 core 的 `emit_token_usage`，含 context_limit / 压缩阈值）。未绑定（turn
    /// 外，或 turn 已结束的迟到上报）时静默丢弃并打 debug 日志。
    fn report(&self, usage: tiangong_types::TokenUsage, source: String) {
        if usage.total_tokens == 0 {
            return;
        }
        let Ok(guard) = self.binding.lock() else {
            return;
        };
        let Some(binding) = guard.as_ref() else {
            tracing::debug!(
                source = %source,
                total_tokens = usage.total_tokens,
                "drop plugin usage: no active turn (late report or outside turn)",
            );
            return;
        };
        if let Ok(mut acc) = binding.usage.lock() {
            acc.accumulate(&usage);
        }
        let compression_threshold =
            crate::react::context::compression_threshold_tokens(binding.context_limit);
        let _ = binding
            .stream_tx
            .send(tiangong_types::StreamEvent::TokenUsage {
                usage,
                current_tokens: None,
                compression_threshold_tokens: Some(compression_threshold),
                context_limit_tokens: Some(binding.context_limit),
                source,
                agent_id: None,
            });
    }
}

impl Default for TurnUsageSink {
    fn default() -> Self {
        Self::new()
    }
}

/// turn-scoped 绑定的 RAII 守卫：drop 时解绑本轮 binding。
///
/// 由 [`TurnUsageSink::bind`] 返回，`execute_turn` 持有至 turn 结束。drop 后迟到的
/// usage 不再被接收（[`TurnUsageSink::report`] 会静默丢弃）。
pub struct TurnUsageGuard {
    sink: Arc<Mutex<Option<TurnUsageBinding>>>,
}

impl Drop for TurnUsageGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.sink.lock() {
            *guard = None;
        }
    }
}

// ── 插件反馈通道 ──────────────────────────────────────────────

/// 插件状态反馈通道的发送端。
///
/// 封装两类投递：
/// - `cmd_tx`：会话注入等走命令队列的事件（由 agent loop drain 处理）；
/// - `usage_sink`：turn-scoped 用量收集器（即时落账，不走命令队列）。
///
/// 插件持有 clone 后即可投递外部事件、子调用 token 用量等。`clone` 与底层
/// `UnboundedSender::clone` 同义，clone 多份指向同一接收端 / 同一 sink。
#[derive(Clone)]
pub struct PluginFeedbackTx {
    tx: UnboundedSender<Command>,
    usage_sink: Arc<TurnUsageSink>,
}

impl PluginFeedbackTx {
    /// 注入一条工具结果到对话上下文（`tool_name` + JSON payload）。
    ///
    /// 走命令队列，由 agent loop drain 时注入 session。通道关闭（worker 已退出）时
    /// 静默丢弃，不报错。
    pub fn inject_tool(&self, tool_name: impl Into<String>, payload: serde_json::Value) {
        let _ = self.tx.send(Command::InjectTool {
            tool_name: tool_name.into(),
            payload,
        });
    }

    /// 上报一笔插件内部产生的 LLM token 用量。
    ///
    /// **即时**累加到本轮 usage 并立即发送 `StreamEvent::TokenUsage`，确保最终
    /// `Done.usage` 包含该消耗。不走命令队列，因此不受 agent loop drain 时机影响，
    /// 也不会被 `check_cancel` 等 drain 路径吞掉。turn 外（或 turn 已结束的迟到上报）
    /// 静默丢弃。
    pub fn report_token_usage(&self, usage: tiangong_types::TokenUsage, source: impl Into<String>) {
        self.usage_sink.report(usage, source.into());
    }

    /// 投递一条流事件（转发到 worker 的 `stream_tx`）。
    ///
    /// 仅向 UI 推送实时事件（如 `MemoryRecallStart` / `MemoryRecallProgress` /
    /// `MemoryRecallDone`），不进入对话历史，也不累加任何 usage。通道关闭时静默丢弃。
    ///
    /// 注意：用于上报 LLM token 用量时应改用 [`report_token_usage`](Self::report_token_usage)，
    /// 后者能让 core 正确即时记账；直接用本方法转发 `StreamEvent::TokenUsage` 只会让
    /// 前端看到事件而不会计入本轮 `Done.usage`。
    pub fn send_stream_event(&self, event: tiangong_types::StreamEvent) {
        let _ = self.tx.send(Command::EmitStreamEvent(event));
    }

    /// 通道是否已关闭（worker 已退出，无法再投递命令队列事件）。
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// 从 core 内部的 `cmd_tx` 与共享 usage sink 构造反馈通道（仅 core 可调用）。
impl PluginFeedbackTx {
    pub(crate) fn new(tx: UnboundedSender<Command>, usage_sink: Arc<TurnUsageSink>) -> Self {
        Self { tx, usage_sink }
    }
}
