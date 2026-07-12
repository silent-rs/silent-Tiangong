//! 插件 → core 的语义反馈通道。
//!
//! 让插件向 core 投递**语义事件**，由 core 统一决定如何处理。插件只描述“发生了
//! 什么”，core 决策“如何处理”（累加 usage / 注入 session / 转发流事件）。
//!
//! 两类投递，链路不同：
//!
//! - **会话注入**（[`PluginFeedbackTx::inject_tool`]）：走 worker 命令队列
//!   （[`Command::InjectTool`]），由 agent loop drain 时注入 session。适合不要求
//!   即时性的外部事件（浏览器页面变化、终端用户操作）。
//!
//! - **用量上报**（[`PluginFeedbackTx::report_token_usage`]）：走 **core 拥有的
//!   turn-scoped usage sink**，即时累加到本轮用量并立即发送 `StreamEvent::TokenUsage`，
//!   **不经过命令队列**。这样插件工具完成 multimodal 子调用时 usage 立即落账，
//!   不依赖 agent loop 何时 drain 命令队列，也不会被 `check_cancel` 等只想检查
//!   取消的 drain 路径吞掉（见 [`TurnUsageSink`] 的作用域说明）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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
    id: u64,
    /// 本轮专用的插件 usage 累加器（turn 结束时并入 accumulated_usage）。
    usage: Mutex<tiangong_types::TokenUsage>,
    /// 即时发送 TokenUsage 流事件。
    stream_tx: std::sync::mpsc::Sender<tiangong_types::StreamEvent>,
    /// 上下文上限（填充 StreamEvent::TokenUsage.context_limit_tokens）。
    context_limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BindingOwner {
    Task(tokio::task::Id),
    Thread(std::thread::ThreadId),
}

fn current_binding_owner() -> BindingOwner {
    tokio::task::try_id()
        .map(BindingOwner::Task)
        .unwrap_or_else(|| BindingOwner::Thread(std::thread::current().id()))
}

/// Turn-scoped 插件 usage 收集器。
///
/// 按 Tokio task（无 task 时按线程）隔离的可嵌套绑定表：turn 开始时 core 绑定本轮，
/// turn 结束时只移除自己的绑定。插件通过 [`PluginFeedbackTx`] 持有共享 sink，
/// 引用，调用 [`TurnUsageSink::report`] 时即时累加并发送——**不经过命令队列**，
/// 因此不受 agent loop drain 时机影响，也不会被 `check_cancel` 等 drain 吞掉。
///
/// 作用域保证：turn 结束后立即解绑，迟到的 usage（如上一轮后台任务迟到上报）会被
/// 静默丢弃并打 debug 日志，不会错误计入下一轮。
///
#[derive(Clone)]
pub struct TurnUsageSink {
    bindings: Arc<Mutex<HashMap<BindingOwner, Vec<TurnUsageBinding>>>>,
    next_binding_id: Arc<AtomicU64>,
}

impl TurnUsageSink {
    /// 构造空的 sink（无 turn 绑定）。
    pub fn new() -> Self {
        Self {
            bindings: Arc::new(Mutex::new(HashMap::new())),
            next_binding_id: Arc::new(AtomicU64::new(1)),
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
        let owner = current_binding_owner();
        let binding_id = self.next_binding_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut guard) = self.bindings.lock() {
            guard.entry(owner).or_default().push(TurnUsageBinding {
                id: binding_id,
                usage: Mutex::new(tiangong_types::TokenUsage::default()),
                stream_tx,
                context_limit,
            });
        }
        TurnUsageGuard {
            bindings: Arc::clone(&self.bindings),
            owner,
            binding_id,
        }
    }

    /// 取出本轮累计的插件 usage（turn 结束时调用，并入 accumulated_usage）。
    ///
    /// 取出后清空本轮累加器（但不解绑绑定，解绑由 [`TurnUsageGuard`] drop 负责）。
    /// 无绑定或无累计时返回 `TokenUsage::default()`。
    pub fn take_usage(&self) -> tiangong_types::TokenUsage {
        self.take_usage_for(current_binding_owner())
    }

    fn take_usage_for(&self, owner: BindingOwner) -> tiangong_types::TokenUsage {
        let Ok(guard) = self.bindings.lock() else {
            return tiangong_types::TokenUsage::default();
        };
        let Some(binding) = guard.get(&owner).and_then(|bindings| bindings.last()) else {
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
    fn record(
        &self,
        owner: BindingOwner,
        usage: tiangong_types::TokenUsage,
        source: String,
        emit_event: bool,
    ) {
        if usage.total_tokens == 0 {
            return;
        }
        let Ok(guard) = self.bindings.lock() else {
            return;
        };
        let Some(binding) = guard.get(&owner).and_then(|bindings| bindings.last()) else {
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
        if !emit_event {
            return;
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

    fn report(&self, owner: BindingOwner, usage: tiangong_types::TokenUsage, source: String) {
        self.record(owner, usage, source, true);
    }

    fn accumulate(&self, owner: BindingOwner, usage: tiangong_types::TokenUsage, source: String) {
        self.record(owner, usage, source, false);
    }

    /// 直接写入当前 turn 的流事件出口，不经过 worker 命令队列。
    fn send_event(&self, owner: BindingOwner, event: tiangong_types::StreamEvent) -> bool {
        let Ok(guard) = self.bindings.lock() else {
            return false;
        };
        let Some(binding) = guard.get(&owner).and_then(|bindings| bindings.last()) else {
            return false;
        };
        binding.stream_tx.send(event).is_ok()
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
    bindings: Arc<Mutex<HashMap<BindingOwner, Vec<TurnUsageBinding>>>>,
    owner: BindingOwner,
    binding_id: u64,
}

impl Drop for TurnUsageGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.bindings.lock()
            && let Some(bindings) = guard.get_mut(&self.owner)
            && let Some(index) = bindings
                .iter()
                .rposition(|binding| binding.id == self.binding_id)
        {
            bindings.remove(index);
            if bindings.is_empty() {
                guard.remove(&self.owner);
            }
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
    turn_owner: Option<BindingOwner>,
}

impl PluginFeedbackTx {
    /// 从 core 内部的命令发送端与共享 usage sink 构造反馈通道。
    pub(crate) fn new(tx: UnboundedSender<Command>, usage_sink: Arc<TurnUsageSink>) -> Self {
        Self {
            tx,
            usage_sink,
            turn_owner: None,
        }
    }

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

    /// 原子提交一组持久投递的完成状态与对应工具注入，并等待父 Session 落盘。
    ///
    /// Core 会在同一次持久化中删除 `delivery_ids` 并把 `tool_injections` 写入
    /// deferred 队列。失败时父 Session 回滚，本方法返回错误；调用方可安全重试。
    pub async fn commit_pending_deliveries(
        &self,
        delivery_ids: Vec<String>,
        tool_injections: Vec<PluginFeedback>,
    ) -> Result<(), String> {
        let (persistence_ack, receiver) = tokio::sync::oneshot::channel();
        self.tx
            .send(Command::CommitPluginDeliveries {
                delivery_ids,
                tool_injections,
                cancelled: false,
                persistence_ack: Some(persistence_ack),
            })
            .map_err(|_| "Core 已关闭，无法提交插件持久投递".to_string())?;
        receiver
            .await
            .map_err(|_| "Core 未返回插件持久投递确认".to_string())?
    }

    /// 无等待地确认一组插件持久投递已经完成。
    ///
    /// 会话更新仍由 Core 所在线程串行执行，但调用方不等待持久化确认。需要同时
    /// 注入处理结果时应使用 [`commit_pending_deliveries`](Self::commit_pending_deliveries)；
    /// 明确取消则使用 [`cancel_pending_deliveries`](Self::cancel_pending_deliveries)。
    pub fn complete_pending_deliveries(&self, delivery_ids: Vec<String>) {
        self.commit_pending_deliveries_without_ack(delivery_ids, Vec::new());
    }

    /// 无等待地提交投递结果；适合生命周期恢复阶段，不能阻塞 Core 的单写 worker。
    pub fn commit_pending_deliveries_without_ack(
        &self,
        delivery_ids: Vec<String>,
        tool_injections: Vec<PluginFeedback>,
    ) {
        if delivery_ids.is_empty() {
            return;
        }
        let _ = self.tx.send(Command::CommitPluginDeliveries {
            delivery_ids,
            tool_injections,
            cancelled: false,
            persistence_ack: None,
        });
    }

    /// 无等待地取消一组插件持久投递，并原子标记来源用户消息为已取消。
    pub fn cancel_pending_deliveries(&self, delivery_ids: Vec<String>) {
        if delivery_ids.is_empty() {
            return;
        }
        let _ = self.tx.send(Command::CommitPluginDeliveries {
            delivery_ids,
            tool_injections: Vec::new(),
            cancelled: true,
            persistence_ack: None,
        });
    }

    /// 上报一笔插件内部产生的 LLM token 用量。
    ///
    /// **即时**累加到本轮 usage 并立即发送 `StreamEvent::TokenUsage`，确保最终
    /// `Done.usage` 包含该消耗。不走命令队列，因此不受 agent loop drain 时机影响，
    /// 也不会被 `check_cancel` 等 drain 路径吞掉。turn 外（或 turn 已结束的迟到上报）
    /// 静默丢弃。
    pub fn report_token_usage(&self, usage: tiangong_types::TokenUsage, source: impl Into<String>) {
        self.usage_sink.report(
            self.turn_owner.unwrap_or_else(current_binding_owner),
            usage,
            source.into(),
        );
    }

    /// 只把用量并入当前 turn，不重复发送 `TokenUsage` 事件。
    ///
    /// 适合嵌套执行已经逐笔转发过用量事件、但还需要把最终合计并入父 turn 的场景。
    pub fn accumulate_token_usage(
        &self,
        usage: tiangong_types::TokenUsage,
        source: impl Into<String>,
    ) {
        self.usage_sink.accumulate(
            self.turn_owner.unwrap_or_else(current_binding_owner),
            usage,
            source.into(),
        );
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
        let _ = self.tx.send(Command::EmitStreamEvent(Box::new(event)));
    }

    /// 立即发送当前工具执行产生的流事件。
    ///
    /// 与 [`send_stream_event`](Self::send_stream_event) 不同，本方法不经过当前 worker
    /// 的命令队列，适用于工具处理器正在等待的嵌套执行输出和审批请求。当前没有
    /// 活跃 turn 时返回 `false`，调用方可回退到普通队列投递。
    pub fn send_turn_stream_event(&self, event: tiangong_types::StreamEvent) -> bool {
        self.usage_sink
            .send_event(self.turn_owner.unwrap_or_else(current_binding_owner), event)
    }

    /// 捕获当前 turn 的绑定归属，供转发线程或嵌套任务继续使用同一实时出口。
    pub fn for_current_turn(mut self) -> Self {
        self.turn_owner = Some(current_binding_owner());
        self
    }

    /// 通道是否已关闭（worker 已退出，无法再投递命令队列事件）。
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};

    use tokio::sync::Barrier;

    use super::*;

    fn usage(prompt_tokens: usize, completion_tokens: usize) -> tiangong_types::TokenUsage {
        tiangong_types::TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }
    }

    fn assert_usage(
        actual: &tiangong_types::TokenUsage,
        prompt_tokens: usize,
        completion_tokens: usize,
    ) {
        assert_eq!(actual.prompt_tokens, prompt_tokens);
        assert_eq!(actual.completion_tokens, completion_tokens);
        assert_eq!(actual.total_tokens, prompt_tokens + completion_tokens);
    }

    fn assert_usage_event(
        event: tiangong_types::StreamEvent,
        prompt_tokens: usize,
        completion_tokens: usize,
        context_limit: usize,
        source: &str,
    ) {
        let tiangong_types::StreamEvent::TokenUsage {
            usage,
            context_limit_tokens,
            source: actual_source,
            ..
        } = event
        else {
            panic!("expected token usage event");
        };
        assert_usage(&usage, prompt_tokens, completion_tokens);
        assert_eq!(context_limit_tokens, Some(context_limit));
        assert_eq!(actual_source, source);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn nested_binding_in_same_task_restores_outer_binding() {
        let sink = TurnUsageSink::new();
        let (outer_tx, outer_rx) = mpsc::channel();
        let outer_guard = sink.bind(outer_tx, 1_000);

        sink.report(current_binding_owner(), usage(2, 3), "outer-before".into());

        let (inner_tx, inner_rx) = mpsc::channel();
        let inner_guard = sink.bind(inner_tx, 2_000);
        sink.report(current_binding_owner(), usage(5, 7), "inner".into());

        let inner_usage = sink.take_usage();
        assert_usage(&inner_usage, 5, 7);
        assert_usage_event(inner_rx.try_recv().unwrap(), 5, 7, 2_000, "inner");

        drop(inner_guard);
        sink.report(current_binding_owner(), usage(11, 13), "outer-after".into());

        let outer_usage = sink.take_usage();
        assert_usage(&outer_usage, 13, 16);
        assert_usage_event(outer_rx.try_recv().unwrap(), 2, 3, 1_000, "outer-before");
        assert_usage_event(outer_rx.try_recv().unwrap(), 11, 13, 1_000, "outer-after");
        assert!(outer_rx.try_recv().is_err());

        drop(outer_guard);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn accumulate_only_does_not_emit_duplicate_usage_event() {
        let sink = TurnUsageSink::new();
        let (stream_tx, stream_rx) = mpsc::channel();
        let _guard = sink.bind(stream_tx, 1_000);

        sink.accumulate(
            current_binding_owner(),
            usage(17, 19),
            "nested-total".into(),
        );

        assert_usage(&sink.take_usage(), 17, 19);
        assert!(stream_rx.try_recv().is_err());
    }

    async fn run_isolated_turn(
        sink: Arc<TurnUsageSink>,
        barrier: Arc<Barrier>,
        prompt_tokens: usize,
        completion_tokens: usize,
        context_limit: usize,
        source: &'static str,
    ) -> (tiangong_types::TokenUsage, tiangong_types::StreamEvent) {
        let (stream_tx, stream_rx) = mpsc::channel();
        let _guard = sink.bind(stream_tx, context_limit);

        barrier.wait().await;
        sink.report(
            current_binding_owner(),
            usage(prompt_tokens, completion_tokens),
            source.into(),
        );
        // 强制让任务可能迁移线程，验证归属跟随 Tokio task，而非执行线程。
        tokio::task::yield_now().await;

        (sink.take_usage(), stream_rx.try_recv().unwrap())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_tasks_keep_usage_and_streams_isolated() {
        let sink = Arc::new(TurnUsageSink::new());
        let barrier = Arc::new(Barrier::new(2));

        let first = tokio::spawn(run_isolated_turn(
            Arc::clone(&sink),
            Arc::clone(&barrier),
            3,
            5,
            3_000,
            "first",
        ));
        let second = tokio::spawn(run_isolated_turn(
            Arc::clone(&sink),
            Arc::clone(&barrier),
            7,
            11,
            4_000,
            "second",
        ));

        let (first, second) = tokio::join!(first, second);
        let (first_usage, first_event) = first.unwrap();
        let (second_usage, second_event) = second.unwrap();

        assert_usage(&first_usage, 3, 5);
        assert_usage_event(first_event, 3, 5, 3_000, "first");
        assert_usage(&second_usage, 7, 11);
        assert_usage_event(second_event, 7, 11, 4_000, "second");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn commit_pending_deliveries_waits_for_core_ack() {
        let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel();
        let feedback = PluginFeedbackTx::new(command_tx, Arc::new(TurnUsageSink::new()));
        let commit = tokio::spawn(async move {
            feedback
                .commit_pending_deliveries(
                    vec!["delivery-1".to_string()],
                    vec![PluginFeedback::new(
                        "agent_report",
                        serde_json::json!({ "content": "done" }),
                    )],
                )
                .await
        });

        let command = command_rx.recv().await.unwrap();
        let Command::CommitPluginDeliveries {
            delivery_ids,
            tool_injections,
            cancelled,
            persistence_ack,
        } = command
        else {
            panic!("expected plugin delivery commit");
        };
        assert_eq!(delivery_ids, ["delivery-1"]);
        assert_eq!(tool_injections.len(), 1);
        assert!(!cancelled);
        assert_eq!(tool_injections[0].tool_name, "agent_report");
        assert!(!commit.is_finished());

        persistence_ack.unwrap().send(Ok(())).unwrap();
        assert!(commit.await.unwrap().is_ok());
    }

    #[test]
    fn cancel_pending_deliveries_sets_explicit_cancel_marker() {
        let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel();
        let feedback = PluginFeedbackTx::new(command_tx, Arc::new(TurnUsageSink::new()));

        feedback.cancel_pending_deliveries(vec!["delivery-cancelled".to_string()]);

        assert!(matches!(
            command_rx.try_recv().unwrap(),
            Command::CommitPluginDeliveries {
                cancelled: true,
                tool_injections,
                ..
            } if tool_injections.is_empty()
        ));
    }
}
