//! 子 Agent 调度：在团队工具 handler 内 await 子 Agent 的 `execute_turn`。
//!
//! 调度模型与插件的其他工具一致——主 Agent 调用 `send_message` / `broadcast_message`
//! 时，handler 内部 await 目标子 Agent 的完整 ReAct turn，子 Agent 的汇报作为
//! ToolResult 返回（主 Agent 当轮即可看到产出）。这与 `recall_memory` 等 await 型
//! 工具完全同构，主 Agent 的工具循环天然阻塞等待。
//!
//! `create_agent` 不触发执行（注册后立即返回，不阻塞）；只有 `send_message` /
//! `broadcast_message` 投递消息到 Idle Agent 时才在 handler 内 await 其执行。
//!
//! 子 Agent ↔ 主 Agent 的通信：
//! - **流事件**：经 `PluginFeedbackTx::send_stream_event` 转发（UI 实时看到子 Agent 输出）。
//! - **token 用量**：过程事件逐笔展示，子 Agent `execute_turn` 的合计只并入父 turn。
//! - **汇报内容**：作为 ToolResult.stdout 直接返回给主 Agent（当轮可见）。
//!
//! 子 Agent ↔ 子 Agent 通信走 message bus（`TeamContext.registry` 收件箱 +
//! active handle 实时注入运行中的 Agent）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tiangong_core::core::command::Command;
use tiangong_core::core::plugin::{PluginFeedbackTx, MAX_PLUGIN_DELIVERIES_PER_COMMIT};
use tiangong_core::model::{TokenUsage, ToolSpec};
use tiangong_core::react::engine::ReactEngine;
use tiangong_core::session::{Message, MessageRole, Session};
use tiangong_types::StreamEvent;

use crate::constants::{
    MAX_AGENTS, SUB_AGENT_MAX_OUTER_ITERATIONS, SUB_AGENT_MAX_TOOL_ROUNDS,
    SUB_AGENT_TOTAL_TOKEN_BUDGET,
};
use crate::lifecycle::{persist_child_session_for_parent_id, prepared_agent_message_for_prompt};
use crate::state::message_bus::AgentMessage;
use crate::state::AgentStatus;
use crate::TeamContext;

const DELIVERY_RECEIPT_MARKER: &str = "[agent-team-delivery-receipt]";
const DELIVERY_RECEIPT_ACK_MARKER: &str = "[agent-team-delivery-receipt-ack]";
/// 单个 Agent 一次返回的完成 ID 使用保守份额，确保最多 8 个 Agent 并发聚合时
/// 仍不会超过 Core 单次提交的安全边界。
const MAX_AGENT_DELIVERIES_PER_TURN: usize = MAX_PLUGIN_DELIVERIES_PER_COMMIT / MAX_AGENTS;
const _: () = assert!(MAX_AGENT_DELIVERIES_PER_TURN > 0);
const _: () =
    assert!(MAX_AGENT_DELIVERIES_PER_TURN * MAX_AGENTS <= MAX_PLUGIN_DELIVERIES_PER_COMMIT);

fn agent_delivery_batch_has_capacity(completed_delivery_count: usize) -> bool {
    completed_delivery_count < MAX_AGENT_DELIVERIES_PER_TURN
}

#[derive(Debug, Serialize, Deserialize)]
struct DeliveryReceipt {
    delivery_id: String,
    report: String,
    #[serde(default)]
    main_messages: Vec<AgentMessage>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeliveryReceiptAck {
    delivery_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingReceiptReplay {
    pub agent_id: String,
    pub delivery_id: String,
    pub report: String,
    pub main_messages: Vec<AgentMessage>,
}

fn parse_delivery_receipt(message: &Message) -> Option<DeliveryReceipt> {
    if !message.model_excluded || message.role != MessageRole::System {
        return None;
    }
    let content = message.text_content();
    let payload = content.strip_prefix(DELIVERY_RECEIPT_MARKER)?;
    serde_json::from_str::<DeliveryReceipt>(payload).ok()
}

fn delivery_receipt(session: &Session, delivery_id: &str) -> Option<DeliveryReceipt> {
    session.messages.iter().rev().find_map(|message| {
        let receipt = parse_delivery_receipt(message)?;
        (receipt.delivery_id == delivery_id).then_some(receipt)
    })
}

fn acknowledged_receipt_ids(session: &Session) -> std::collections::HashSet<String> {
    session
        .messages
        .iter()
        .filter(|message| message.model_excluded && message.role == MessageRole::System)
        .filter_map(|message| {
            let content = message.text_content();
            let payload = content.strip_prefix(DELIVERY_RECEIPT_ACK_MARKER)?;
            serde_json::from_str::<DeliveryReceiptAck>(payload).ok()
        })
        .flat_map(|ack| ack.delivery_ids)
        .collect()
}

/// 扫描 child sessions 中尚未由父 Core 确认或取消的持久回执。
pub(crate) fn pending_receipt_replays(
    team: &TeamContext,
    parent_completed_ids: &[String],
    cancelled_ids: &[String],
    settled_ids: &[String],
) -> Vec<PendingReceiptReplay> {
    let excluded = parent_completed_ids
        .iter()
        .chain(cancelled_ids)
        .chain(settled_ids)
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut agent_ids = team
        .registry
        .alive_agents()
        .into_iter()
        .map(|agent| agent.agent_id.clone())
        .collect::<Vec<_>>();
    agent_ids.sort();

    let mut replays = Vec::new();
    for agent_id in agent_ids {
        let Some(session) = team.registry.get_session(&agent_id) else {
            continue;
        };
        let acknowledged = acknowledged_receipt_ids(session);
        let mut seen = std::collections::HashSet::new();
        for receipt in session
            .messages
            .iter()
            .rev()
            .filter_map(parse_delivery_receipt)
        {
            if excluded.contains(receipt.delivery_id.as_str())
                || acknowledged.contains(&receipt.delivery_id)
                || !seen.insert(receipt.delivery_id.clone())
            {
                continue;
            }
            replays.push(PendingReceiptReplay {
                agent_id: agent_id.clone(),
                delivery_id: receipt.delivery_id,
                report: receipt.report,
                main_messages: receipt.main_messages,
            });
        }
    }
    replays.sort_by(|left, right| {
        left.agent_id
            .cmp(&right.agent_id)
            .then_with(|| left.delivery_id.cmp(&right.delivery_id))
    });
    replays
}

fn collect_unreceipted_main_messages(
    team: &TeamContext,
    session: &Session,
    delivery_id: &str,
) -> Vec<AgentMessage> {
    let receipted_ids = session
        .messages
        .iter()
        .filter_map(parse_delivery_receipt)
        .flat_map(|receipt| receipt.main_messages)
        .map(|message| message.id)
        .collect::<std::collections::HashSet<_>>();
    team.main_messages_for_work(delivery_id)
        .into_iter()
        .filter(|message| !receipted_ids.contains(&message.id))
        .collect()
}

fn restore_receipt_main_messages(team: &mut TeamContext, messages: &[AgentMessage]) {
    let mut existing = team
        .main_inbox
        .iter()
        .map(|message| message.id.clone())
        .collect::<std::collections::HashSet<_>>();
    team.main_inbox.extend(
        messages
            .iter()
            .filter(|message| existing.insert(message.id.clone()))
            .cloned(),
    );
}

/// 父 Core 已确认工作结果后，按稳定消息 ID 从内存 outbox 删除对应消息。
///
/// ACK 前调用方必须保留这些消息；若提交失败或超时，回执重放仍会返回同一批消息。
pub(crate) fn remove_acknowledged_main_messages(
    team: &Arc<Mutex<TeamContext>>,
    message_ids: &[String],
) -> usize {
    if message_ids.is_empty() {
        return 0;
    }
    let mut team = team.lock().unwrap_or_else(|poison| poison.into_inner());
    team.remove_main_messages_by_ids(message_ids)
}

fn append_delivery_receipt(
    session: &mut Session,
    delivery_id: &str,
    report: &str,
    main_messages: &[AgentMessage],
) -> Result<(), String> {
    let payload = serde_json::to_string(&DeliveryReceipt {
        delivery_id: delivery_id.to_string(),
        report: report.to_string(),
        main_messages: main_messages.to_vec(),
    })
    .map_err(|error| format!("持久投递回执序列化失败：{error}"))?;
    let mut receipt = Message::new(
        MessageRole::System,
        format!("{DELIVERY_RECEIPT_MARKER}{payload}"),
    );
    receipt.model_excluded = true;
    session.messages.push(receipt);
    Ok(())
}

/// 子 Agent system prompt 构建所需的配置快照（由插件在 register 时捕获）。
///
/// `base` 经 `Arc` 共享（`SystemPromptConfig` 未实现 Clone，且体积较大）。
pub struct PromptConfig {
    /// 当前会话 id（持久化 child session 用）。
    pub session_id: String,
    /// 基础 system prompt 配置（由 `SystemPromptConfig::from_configs` 构建）。
    pub base: Arc<tiangong_core::prompt::SystemPromptConfig>,
}

/// 子 Agent turn 的执行结果。
pub struct AgentTurnResult {
    /// 子 Agent 汇报内容（作为 ToolResult.stdout 返回主 Agent）。
    pub report: String,
    /// 子 Agent 执行消耗的 token 总量（主 Agent 将其并入父 turn）。
    pub usage: TokenUsage,
    /// 是否被取消。
    pub cancelled: bool,
    /// 已由子会话成功落盘确认的稳定工作 ID（即 `AgentInboxEntry.message.id`）。
    pub completed_delivery_ids: Vec<String>,
    /// 本轮子 Agent 发给主 Agent、并随持久投递回执一起落盘的消息。
    pub main_messages: Vec<AgentMessage>,
    /// 是否成功消费并持久化了一条收件箱消息。
    pub(crate) made_progress: bool,
}

/// 嵌套执行被父 Future 丢弃时，立即中止其 Tokio 任务，避免 LLM 继续脱离运行。
struct AbortOnDrop<T> {
    handle: tokio::task::JoinHandle<T>,
    completed: bool,
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if !self.completed {
            self.handle.abort();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BudgetCharge {
    used_tokens: usize,
    exhausted: bool,
    newly_exhausted: bool,
}

/// 单个执行波次内所有子 Agent 共享的 token 预算。
///
/// 实际用量通过 CAS 饱和累加，确保并发完成不会丢更新或把预算计数写过上限。
pub struct SubAgentTokenBudget {
    used_tokens: AtomicUsize,
    paused: AtomicBool,
}

impl SubAgentTokenBudget {
    pub(crate) fn new() -> Self {
        Self {
            used_tokens: AtomicUsize::new(0),
            paused: AtomicBool::new(false),
        }
    }

    pub(crate) fn used_tokens(&self) -> usize {
        self.used_tokens.load(Ordering::Acquire)
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire) || self.used_tokens() >= SUB_AGENT_TOTAL_TOKEN_BUDGET
    }

    pub(crate) fn record_usage(&self, tokens: usize) -> BudgetCharge {
        loop {
            let current = self.used_tokens.load(Ordering::Acquire);
            if current >= SUB_AGENT_TOTAL_TOKEN_BUDGET {
                let newly_exhausted = !self.paused.swap(true, Ordering::AcqRel);
                return BudgetCharge {
                    used_tokens: SUB_AGENT_TOTAL_TOKEN_BUDGET,
                    exhausted: true,
                    newly_exhausted,
                };
            }
            if tokens == 0 {
                return BudgetCharge {
                    used_tokens: current,
                    exhausted: false,
                    newly_exhausted: false,
                };
            }
            let next = current
                .saturating_add(tokens)
                .min(SUB_AGENT_TOTAL_TOKEN_BUDGET);
            if self
                .used_tokens
                .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            let exhausted = next >= SUB_AGENT_TOTAL_TOKEN_BUDGET;
            let newly_exhausted = exhausted && !self.paused.swap(true, Ordering::AcqRel);
            return BudgetCharge {
                used_tokens: next,
                exhausted,
                newly_exhausted,
            };
        }
    }

    /// 仅在团队没有运行中执行时，由新的主会话 turn 重置下一波预算。
    pub(crate) fn reset(&self) -> bool {
        let was_paused = self.paused.swap(false, Ordering::AcqRel);
        self.used_tokens.store(0, Ordering::Release);
        was_paused
    }
}

impl Default for SubAgentTokenBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// 子 Agent 已经通过流事件上报、但尚未合并进父 turn 的用量。
///
/// 流事件会立即扣减团队共享预算；正常完成时仍由
/// `AgentTurnResult::usage` 一次性合并到父 turn。只有执行 Future 被丢弃或
/// 子任务异常退出时，才从这里回收已观测但未合并的用量。
#[derive(Default)]
struct ObservedSubAgentUsageState {
    usage: TokenUsage,
    budget_accounted_tokens: usize,
}

#[derive(Clone, Default)]
struct ObservedSubAgentUsage {
    state: Arc<Mutex<ObservedSubAgentUsageState>>,
}

impl ObservedSubAgentUsage {
    #[cfg(test)]
    fn record(&self, usage: &TokenUsage) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .usage
            .accumulate(usage);
    }

    /// 记录一条流式 usage，返回本事件尚未观测的增量。
    ///
    /// 普通事件是逐次调用增量；只有显式的 `cancelled-cumulative` 事件携带当前
    /// child turn 的累计值。`cancelled-incremental` 与其他普通事件一样直接累加。
    /// `turn_observed` 由单个转发线程持有，避免同一 Agent 连续执行多个 turn 时把
    /// 新 turn 的累计值与历史总量错误比较。
    fn record_stream_event(
        &self,
        usage: &TokenUsage,
        cumulative: bool,
        turn_observed: &mut TokenUsage,
    ) -> TokenUsage {
        let delta = if cumulative {
            let delta = usage_delta(usage, turn_observed);
            merge_cumulative_usage(turn_observed, usage);
            delta
        } else {
            turn_observed.accumulate(usage);
            usage.clone()
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.usage.accumulate(&delta);
        // 与 usage 记录在同一临界区内标记，避免中断回收与转发线程重复扣减。
        state.budget_accounted_tokens = state
            .budget_accounted_tokens
            .saturating_add(delta.total_tokens);
        delta
    }

    fn total_tokens(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .usage
            .total_tokens
    }

    fn mark_budget_accounted(&self, tokens: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.budget_accounted_tokens = state.budget_accounted_tokens.saturating_add(tokens);
    }

    fn budget_accounted_tokens(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .budget_accounted_tokens
    }

    fn take(&self) -> TokenUsage {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::mem::take(&mut state.usage)
    }
}

fn usage_delta(cumulative: &TokenUsage, observed: &TokenUsage) -> TokenUsage {
    TokenUsage {
        prompt_tokens: cumulative
            .prompt_tokens
            .saturating_sub(observed.prompt_tokens),
        completion_tokens: cumulative
            .completion_tokens
            .saturating_sub(observed.completion_tokens),
        total_tokens: cumulative
            .total_tokens
            .saturating_sub(observed.total_tokens),
        prompt_cache_hit_tokens: cumulative.prompt_cache_hit_tokens.map(|value| {
            value.saturating_sub(observed.prompt_cache_hit_tokens.unwrap_or_default())
        }),
        prompt_cache_miss_tokens: cumulative.prompt_cache_miss_tokens.map(|value| {
            value.saturating_sub(observed.prompt_cache_miss_tokens.unwrap_or_default())
        }),
    }
}

fn merge_cumulative_usage(observed: &mut TokenUsage, cumulative: &TokenUsage) {
    observed.prompt_tokens = observed.prompt_tokens.max(cumulative.prompt_tokens);
    observed.completion_tokens = observed.completion_tokens.max(cumulative.completion_tokens);
    observed.total_tokens = observed
        .total_tokens
        .max(cumulative.total_tokens)
        .max(observed.prompt_tokens + observed.completion_tokens);
    observed.prompt_cache_hit_tokens = max_optional_usage(
        observed.prompt_cache_hit_tokens,
        cumulative.prompt_cache_hit_tokens,
    );
    observed.prompt_cache_miss_tokens = max_optional_usage(
        observed.prompt_cache_miss_tokens,
        cumulative.prompt_cache_miss_tokens,
    );
}

fn max_optional_usage(current: Option<usize>, cumulative: Option<usize>) -> Option<usize> {
    match (current, cumulative) {
        (Some(current), Some(cumulative)) => Some(current.max(cumulative)),
        (current, cumulative) => current.or(cumulative),
    }
}

struct SubAgentUsageRecoveryState {
    observed: ObservedSubAgentUsage,
    armed: bool,
}

impl SubAgentUsageRecoveryState {
    fn new() -> Self {
        Self {
            observed: ObservedSubAgentUsage::default(),
            armed: true,
        }
    }

    fn observer(&self) -> ObservedSubAgentUsage {
        self.observed.clone()
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn take_for_recovery(&mut self) -> (TokenUsage, usize) {
        if !self.armed {
            return (TokenUsage::default(), 0);
        }
        self.armed = false;
        let budget_accounted_tokens = self.observed.budget_accounted_tokens();
        let usage = self.observed.take();
        let unaccounted_tokens = usage.total_tokens.saturating_sub(budget_accounted_tokens);
        (usage, unaccounted_tokens)
    }
}

struct SubAgentUsageRecovery {
    state: SubAgentUsageRecoveryState,
    feedback_tx: PluginFeedbackTx,
    token_budget: Arc<SubAgentTokenBudget>,
    team: Arc<Mutex<TeamContext>>,
    agent_id: String,
}

impl SubAgentUsageRecovery {
    fn new(
        feedback_tx: PluginFeedbackTx,
        token_budget: Arc<SubAgentTokenBudget>,
        team: Arc<Mutex<TeamContext>>,
        agent_id: String,
    ) -> Self {
        Self {
            state: SubAgentUsageRecoveryState::new(),
            feedback_tx,
            token_budget,
            team,
            agent_id,
        }
    }

    fn observer(&self) -> ObservedSubAgentUsage {
        self.state.observer()
    }

    fn disarm(&mut self) {
        self.state.disarm();
    }
}

impl Drop for SubAgentUsageRecovery {
    fn drop(&mut self) {
        let (usage, unaccounted_tokens) = self.state.take_for_recovery();
        self.feedback_tx
            .accumulate_token_usage(usage, "sub_agent_interrupted");
        let charge = self.token_budget.record_usage(unaccounted_tokens);
        pause_team_for_budget(&self.team, &self.agent_id, &self.feedback_tx, charge);
    }
}

fn pause_team_for_budget(
    team: &Arc<Mutex<TeamContext>>,
    _current_agent_id: &str,
    feedback_tx: &PluginFeedbackTx,
    charge: BudgetCharge,
) {
    if !charge.exhausted || !charge.newly_exhausted {
        return;
    }
    let handles = team
        .lock()
        .map(|team| {
            team.active_agent_ids()
                .into_iter()
                .filter_map(|agent_id| {
                    team.active_agent_handle(&agent_id)
                        .map(|handle| (agent_id, handle))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (_agent_id, handle) in handles {
        // 预算暂停不是用户取消：复用 shutdown 语义让当前 entry 回队，下一主 turn 恢复。
        handle.shutdown_flag.store(true, Ordering::Release);
        let _ = handle.command_tx.send(Command::Shutdown);
    }
    emit_feedback_event(
        feedback_tx,
        StreamEvent::AgentNotification {
            agent_id: "agent-team-budget".to_string(),
            agent_label: "Agent Team".to_string(),
            content: format!(
                "Sub Agent 共享 token 预算已达到 {}，剩余任务已暂停；下一次主会话轮次开始且团队空闲时将自动恢复。",
                charge.used_tokens
            ),
            level: "warning".to_string(),
        },
    );
}

/// 无论正常返回、取消还是 panic，都清理运行句柄和 Agent 状态。
struct ActiveAgentCleanup {
    team: Arc<Mutex<TeamContext>>,
    agent_id: String,
    agent_label: String,
    feedback_tx: PluginFeedbackTx,
    entry: Option<crate::state::message_bus::AgentInboxEntry>,
    cancel_flag: Arc<AtomicBool>,
    shutdown_flag: Arc<AtomicBool>,
    armed: bool,
}

impl ActiveAgentCleanup {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

fn cleanup_active_attempt(
    team: &mut TeamContext,
    agent_id: &str,
    entry: &mut Option<crate::state::message_bus::AgentInboxEntry>,
    cancelled: bool,
    shutdown: bool,
) -> Vec<String> {
    team.unregister_active_agent(agent_id);
    if let Some(entry) = entry.as_ref() {
        // 未完成尝试产生的 main outbox 不能泄漏到同一 Agent 的下一条工作。
        team.remove_main_messages_for_work(&entry.message.id);
    }
    // 会话关闭或执行 Future 意外被丢弃时，把尚未确认的消息放回队首；
    // 用户显式取消时则按取消语义丢弃，不在下次启动时重新执行。
    if shutdown || !cancelled {
        if let Some(entry) = entry.take() {
            team.registry.requeue_inbox_entry_front(agent_id, entry);
        }
    }
    team.registry.update_status(agent_id, AgentStatus::Idle);
    team.file_locks.release_all(agent_id)
}

impl Drop for ActiveAgentCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut team = self
            .team
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let released_paths = cleanup_active_attempt(
            &mut team,
            &self.agent_id,
            &mut self.entry,
            self.cancel_flag.load(Ordering::Acquire),
            self.shutdown_flag.load(Ordering::Acquire),
        );
        drop(team);
        for path in released_paths {
            emit_feedback_event(
                &self.feedback_tx,
                StreamEvent::FileLockChanged {
                    path,
                    holder_agent_id: Some(self.agent_id.clone()),
                    holder_agent_label: Some(self.agent_label.clone()),
                    action: "unlocked".to_string(),
                },
            );
        }
    }
}

/// 运行指定 Idle 子 Agent 的一个完整 turn，await 其完成后返回汇报。
///
/// 在 `send_message` / `broadcast_message` 的 handler 内 await 调用。主 Agent 的
/// 工具循环因此阻塞在此，直到子 Agent 完成（与 recall_memory 等 await 型工具一致）。
///
/// 本函数执行：
/// 1. 锁 team，取出收件箱消息 + child_session，置 Running，并注册 active handle
///    （供 cancel 路由）。
/// 2. 构造子 ReactEngine（共享父 RuntimeEngine，继承 tool_overrides），发送首条
///    Command::Message（累积的 inbox 内容），`select!` 同时 await execute_turn 与
///    cancel 信号。
/// 3. 完成后回写 child_session、置 Idle、持久化、注销 cmd_tx。
///
/// 若 Agent 已是 Running（active handle 命中），消息经 dispatch_agent_message
/// 已实时注入其当前循环，本函数不重复启动，返回空汇报。
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_turn(
    team: Arc<Mutex<TeamContext>>,
    agent_id: String,
    storage_root: PathBuf,
    runtime_engine: tiangong_core::runtime::RuntimeEngine,
    parent_tools: Vec<ToolSpec>,
    feedback_tx: PluginFeedbackTx,
    prompt_config: Arc<PromptConfig>,
    execution_semaphore: Arc<tokio::sync::Semaphore>,
    token_budget: Arc<SubAgentTokenBudget>,
) -> AgentTurnResult {
    let mut usage_recovery = SubAgentUsageRecovery::new(
        feedback_tx.clone(),
        Arc::clone(&token_budget),
        Arc::clone(&team),
        agent_id.clone(),
    );
    let observed_usage = usage_recovery.observer();
    let mut usage_recovery_required = false;
    let mut reports = Vec::new();
    let mut usage = TokenUsage::default();
    let mut cancelled = false;
    let mut completed_delivery_ids = Vec::new();
    let mut main_messages = Vec::new();
    let mut made_progress = false;

    loop {
        // 回执重放不消耗 token，也必须遵守同一完成账本批次边界。到达上限时不再
        // 领取下一条 entry，剩余 inbox 留给 scheduler 在本批 ACK 后继续提交。
        if !agent_delivery_batch_has_capacity(completed_delivery_ids.len()) {
            break;
        }
        let result = run_one_agent_turn(
            Arc::clone(&team),
            agent_id.clone(),
            storage_root.clone(),
            runtime_engine.clone(),
            parent_tools.clone(),
            feedback_tx.clone(),
            Arc::clone(&prompt_config),
            Arc::clone(&execution_semaphore),
            Arc::clone(&token_budget),
            observed_usage.clone(),
            &mut usage_recovery_required,
        )
        .await;
        let iteration_made_progress = result.made_progress;
        if !result.report.trim().is_empty() {
            reports.push(result.report);
        }
        usage.accumulate(&result.usage);
        cancelled |= result.cancelled;
        completed_delivery_ids.extend(result.completed_delivery_ids);
        main_messages.extend(result.main_messages);
        made_progress |= iteration_made_progress;

        if usage_recovery_required {
            // Drop 时由逐笔观测值补回父 turn；返回值必须清零，避免 handler 再次累计。
            usage = TokenUsage::default();
            break;
        }
        if cancelled || !iteration_made_progress {
            break;
        }
        if token_budget.is_paused() {
            break;
        }
        let has_more = team
            .lock()
            .map(|team| team.registry.has_pending_inbox_for(&agent_id))
            .unwrap_or(false);
        if !has_more {
            break;
        }
    }

    let result = AgentTurnResult {
        report: reports.join("\n\n"),
        usage,
        cancelled,
        completed_delivery_ids,
        main_messages,
        made_progress,
    };
    if !usage_recovery_required {
        usage_recovery.disarm();
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_one_agent_turn(
    team: Arc<Mutex<TeamContext>>,
    agent_id: String,
    storage_root: PathBuf,
    runtime_engine: tiangong_core::runtime::RuntimeEngine,
    parent_tools: Vec<ToolSpec>,
    feedback_tx: PluginFeedbackTx,
    prompt_config: Arc<PromptConfig>,
    execution_semaphore: Arc<tokio::sync::Semaphore>,
    token_budget: Arc<SubAgentTokenBudget>,
    observed_usage: ObservedSubAgentUsage,
    usage_recovery_required: &mut bool,
) -> AgentTurnResult {
    if token_budget.is_paused() {
        return AgentTurnResult {
            report: "Sub Agent 共享 token 预算已达到上限，任务已暂停并保留到下一次主会话轮次"
                .to_string(),
            usage: TokenUsage::default(),
            cancelled: false,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    }
    let Ok(_permit) = execution_semaphore.acquire_owned().await else {
        return AgentTurnResult {
            report: "Sub Agent 调度已关闭".to_string(),
            usage: TokenUsage::default(),
            cancelled: true,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    };
    if token_budget.is_paused() {
        return AgentTurnResult {
            report: "Sub Agent 共享 token 预算已达到上限，任务已暂停并保留到下一次主会话轮次"
                .to_string(),
            usage: TokenUsage::default(),
            cancelled: false,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    }

    // 在领取收件箱 entry 之前创建控制句柄；领取与 active 注册必须处于同一锁区，
    // 避免 Shutdown/Cancel 落在“已出队但尚不可寻址”的窗口。
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let child_cancel_flag = Arc::new(AtomicBool::new(false));
    let child_shutdown_flag = Arc::new(AtomicBool::new(false));

    // 1. 锁 team，按顺序取一条消息，同时取得会话、登记运行句柄并置 Running。
    let (
        entry,
        mut child_session,
        agent_label,
        agent_role,
        agent_system_prompt,
        team_roster,
        completed_delivery_id,
    ) = {
        let Ok(mut team) = team.lock() else {
            return AgentTurnResult {
                report: "团队状态锁定失败".to_string(),
                usage: TokenUsage::default(),
                cancelled: false,
                completed_delivery_ids: Vec::new(),
                main_messages: Vec::new(),
                made_progress: false,
            };
        };
        // 若 Agent 已在运行，消息已实时注入，不重复启动。
        if team.is_agent_active(&agent_id) {
            return AgentTurnResult {
                report: format!(
                    "{agent_label_running} 正在执行，消息已实时送达",
                    agent_label_running = team
                        .registry
                        .get(&agent_id)
                        .map(|d| d.label.as_str())
                        .unwrap_or("Agent")
                ),
                usage: TokenUsage::default(),
                cancelled: false,
                completed_delivery_ids: Vec::new(),
                main_messages: Vec::new(),
                made_progress: false,
            };
        }
        let Some(child_session) = team.registry.get_session(&agent_id).cloned() else {
            return AgentTurnResult {
                report: "子 Agent 会话缺失".to_string(),
                usage: TokenUsage::default(),
                cancelled: false,
                completed_delivery_ids: Vec::new(),
                main_messages: Vec::new(),
                made_progress: false,
            };
        };
        let (label, role, system_prompt) = match team.registry.get(&agent_id) {
            Some(d) => (d.label.clone(), d.role.clone(), d.system_prompt.clone()),
            None => {
                return AgentTurnResult {
                    report: "子 Agent 已注销".to_string(),
                    usage: TokenUsage::default(),
                    cancelled: false,
                    completed_delivery_ids: Vec::new(),
                    main_messages: Vec::new(),
                    made_progress: false,
                };
            }
        };
        let Some(entry) = team.registry.take_next_inbox_entry(&agent_id) else {
            return AgentTurnResult {
                report: "无待处理消息".to_string(),
                usage: TokenUsage::default(),
                cancelled: false,
                completed_delivery_ids: Vec::new(),
                main_messages: Vec::new(),
                made_progress: false,
            };
        };
        // 每条收件箱消息都以自身稳定 ID 作为父 Session 提交键。用户直达消息还会
        // 同时移除 pending；普通内部消息则依靠完成账本让报告 ACK 可安全重试。
        let completed_delivery_id = Some(entry.message.id.clone());
        team.registry.update_status(&agent_id, AgentStatus::Running);
        team.register_active_agent(
            agent_id.clone(),
            cmd_tx.clone(),
            Arc::clone(&child_cancel_flag),
            Arc::clone(&child_shutdown_flag),
            completed_delivery_id.clone(),
        );
        emit_feedback_event(
            &feedback_tx,
            StreamEvent::AgentStatusChanged {
                agent_id: agent_id.clone(),
                label: label.clone(),
                status: "running".to_string(),
            },
        );
        // 渲染团队花名册（供子 Agent system prompt 上下文）。
        let roster = format_team_roster(&team);
        (
            entry,
            child_session,
            label,
            role,
            system_prompt,
            roster,
            completed_delivery_id,
        )
    };
    let mut active_cleanup = ActiveAgentCleanup {
        team: Arc::clone(&team),
        agent_id: agent_id.clone(),
        agent_label: agent_label.clone(),
        feedback_tx: feedback_tx.clone(),
        entry: Some(entry.clone()),
        cancel_flag: Arc::clone(&child_cancel_flag),
        shutdown_flag: Arc::clone(&child_shutdown_flag),
        armed: true,
    };

    // 上次执行可能已把回执与 child session 原子落盘，只是父 Session 尚未来得及
    // 确认该稳定工作 ID。用户直达与普通内部消息都直接重放，绝不再次调用 LLM。
    if let Some(delivery_id) = completed_delivery_id.as_deref() {
        if let Some(receipt) = delivery_receipt(&child_session, delivery_id) {
            if child_shutdown_flag.load(Ordering::Acquire) {
                return AgentTurnResult {
                    report: format!("[{agent_label}] 会话关闭，已完成投递保留待下次确认"),
                    usage: TokenUsage::default(),
                    cancelled: true,
                    completed_delivery_ids: Vec::new(),
                    main_messages: Vec::new(),
                    made_progress: false,
                };
            }
            if child_cancel_flag.load(Ordering::Acquire) {
                return AgentTurnResult {
                    report: format!("[{agent_label}] 本轮已取消"),
                    usage: TokenUsage::default(),
                    cancelled: true,
                    completed_delivery_ids: Vec::new(),
                    main_messages: Vec::new(),
                    made_progress: false,
                };
            }
            let mut locked_team = team.lock().unwrap_or_else(|poison| poison.into_inner());
            // Cancel/Shutdown 在同一把 team 锁内置位；锁内复查决定 receipt 重放与
            // 取消谁先线性化，避免已持久 tombstone 的工作仍被报告为成功。
            if child_shutdown_flag.load(Ordering::Acquire) {
                drop(locked_team);
                return AgentTurnResult {
                    report: format!("[{agent_label}] 会话关闭，已完成投递保留待下次确认"),
                    usage: TokenUsage::default(),
                    cancelled: true,
                    completed_delivery_ids: Vec::new(),
                    main_messages: Vec::new(),
                    made_progress: false,
                };
            }
            if child_cancel_flag.load(Ordering::Acquire) {
                drop(locked_team);
                return AgentTurnResult {
                    report: format!("[{agent_label}] 本轮已取消"),
                    usage: TokenUsage::default(),
                    cancelled: true,
                    completed_delivery_ids: Vec::new(),
                    main_messages: Vec::new(),
                    made_progress: false,
                };
            }
            restore_receipt_main_messages(&mut locked_team, &receipt.main_messages);
            locked_team
                .registry
                .update_status(&agent_id, AgentStatus::Idle);
            locked_team.unregister_active_agent(&agent_id);
            drop(locked_team);
            active_cleanup.disarm();
            emit_feedback_event(
                &feedback_tx,
                StreamEvent::AgentStatusChanged {
                    agent_id: agent_id.clone(),
                    label: agent_label,
                    status: "idle".to_string(),
                },
            );
            return AgentTurnResult {
                report: receipt.report,
                usage: TokenUsage::default(),
                cancelled: false,
                completed_delivery_ids: vec![delivery_id.to_string()],
                main_messages: receipt.main_messages,
                made_progress: true,
            };
        }
    }

    let start_message_len = child_session.messages.len();
    let child_message_id = entry
        .session_message_id
        .clone()
        .unwrap_or_else(|| entry.message.id.clone());
    let prepared =
        prepared_agent_message_for_prompt(&entry.message, entry.additional_content.clone());

    // 2. 构造子 ReactEngine + cmd 通道。
    // TurnUsageSink 使用可嵌套绑定；子 Agent 进入时压栈，结束后恢复父 Agent 绑定。
    let sub_tools = filter_sub_agent_tools(parent_tools, &agent_id, &team);
    let sub_runtime = runtime_engine;

    // 构建子 Agent 的 system prompt（角色指令 + 团队花名册 + 基础配置）。
    let system_prompt = {
        let additional_context = if team_roster.is_empty() {
            String::new()
        } else {
            format!("当前团队成员：\n{team_roster}")
        };
        let ctx = tiangong_core::prompt::ScopedSystemPromptContext::new(
            prompt_config.base.as_ref(),
            &agent_system_prompt,
            &additional_context,
        );
        ctx.build(&child_session)
    };
    child_session.system_prompt_message = Some(system_prompt.clone());

    let mut sub_engine = ReactEngine::new(
        sub_runtime,
        sub_tools,
        SUB_AGENT_MAX_TOOL_ROUNDS,
        SUB_AGENT_MAX_OUTER_ITERATIONS,
    )
    .with_agent_id(agent_id.clone())
    .with_system_prompt(system_prompt)
    .with_cancel_flag(Arc::clone(&child_cancel_flag))
    .with_shutdown_flag(Arc::clone(&child_shutdown_flag));

    if cmd_tx
        .send(Command::Message {
            prepared,
            message_id: Some(child_message_id),
            trust_mode_override: None,
            persistence_ack: None,
        })
        .is_err()
    {
        return AgentTurnResult {
            report: format!("[{agent_label}] 子 Agent 消息通道不可用，消息已回队"),
            usage: TokenUsage::default(),
            cancelled: false,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    }

    // 子 Agent stream 事件经独立 channel + 转发线程 → feedback。
    let (child_stream_tx, child_stream_rx) = std::sync::mpsc::channel::<StreamEvent>();
    let observed_before = observed_usage.total_tokens();
    let forwarder = spawn_sub_agent_stream_forwarder(
        agent_id.clone(),
        agent_role,
        agent_label.clone(),
        feedback_tx.clone(),
        Arc::clone(&team),
        Arc::clone(&token_budget),
        observed_usage.clone(),
        child_stream_rx,
    );

    // 3. 在独立 Tokio 任务中执行；父 Future 被取消时 AbortOnDrop 会中止子任务。
    let keep_cmd_tx = cmd_tx;
    let mut child_task = AbortOnDrop {
        handle: tokio::spawn(async move {
            let usage = sub_engine
                .execute_turn(&mut child_session, None, &child_stream_tx, &mut cmd_rx)
                .await;
            (usage, child_session)
        }),
        completed: false,
    };
    let joined = (&mut child_task.handle).await;
    child_task.completed = true;
    drop(keep_cmd_tx);
    let _ = forwarder.join();
    let (usage, mut child_session) = match joined {
        Ok(output) => output,
        Err(error) => {
            *usage_recovery_required = true;
            return AgentTurnResult {
                report: format!("[{agent_label}] 子 Agent 执行异常，消息已回队：{error}"),
                usage: TokenUsage::default(),
                cancelled: error.is_cancelled(),
                completed_delivery_ids: Vec::new(),
                main_messages: Vec::new(),
                made_progress: false,
            };
        }
    };
    // 正常情况下 usage 已由流事件即时扣减；只补记未通过流上报的尾差。
    let observed_delta = observed_usage
        .total_tokens()
        .saturating_sub(observed_before);
    let unobserved_tokens = usage.total_tokens.saturating_sub(observed_delta);
    if unobserved_tokens > 0 {
        observed_usage.mark_budget_accounted(unobserved_tokens);
        let budget_charge = token_budget.record_usage(unobserved_tokens);
        pause_team_for_budget(&team, &agent_id, &feedback_tx, budget_charge);
    }

    // Shutdown 不是用户取消。关闭期间不得确认这条持久投递，也不能保存半轮结果；
    // 保留原 entry，待下次启动按稳定 delivery id 恢复执行。
    if child_shutdown_flag.load(Ordering::Acquire) {
        return AgentTurnResult {
            report: format!("[{agent_label}] 会话关闭，未完成消息已保留待恢复"),
            usage,
            cancelled: true,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    }
    // 显式取消已由插件通知 Core 删除 pending delivery；不回队、不生成成功回执。
    // 保持 cleanup armed，让它释放锁并发送对应的 unlocked 事件。
    if child_cancel_flag.load(Ordering::Acquire) {
        return AgentTurnResult {
            report: format!("[{agent_label}] 本轮已取消"),
            usage,
            cancelled: true,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    }

    // 4. 快照本工作尚未写入其他回执的 main 消息。消息与回执随 child session
    // 原子落盘，但在父 Core ACK 前继续保留于内存 outbox。
    let report = build_agent_report(&child_session, start_message_len, &agent_label);
    let main_messages = completed_delivery_id
        .as_deref()
        .and_then(|delivery_id| {
            team.lock()
                .ok()
                .map(|team| collect_unreceipted_main_messages(&team, &child_session, delivery_id))
        })
        .unwrap_or_default();
    let status = if report.contains("执行出错") {
        "error"
    } else {
        "idle"
    };
    let receipt_result = completed_delivery_id
        .as_deref()
        .map_or(Ok(()), |delivery_id| {
            append_delivery_receipt(&mut child_session, delivery_id, &report, &main_messages)
        });
    let persist_result = receipt_result
        .and_then(|()| {
            child_session
                .parent_session_id
                .clone()
                .ok_or_else(|| "子 Agent 会话缺少父会话 ID".to_string())
        })
        .and_then(|parent_id| {
            persist_child_session_for_parent_id(
                &storage_root,
                &parent_id,
                &agent_id,
                &child_session,
            )
        });
    if let Err(error) = persist_result {
        let error_report = format!("[{agent_label}] 子 Agent 会话保存失败，消息已回队：{error}");
        emit_feedback_event(
            &feedback_tx,
            StreamEvent::AgentNotification {
                agent_id: agent_id.clone(),
                agent_label: agent_label.clone(),
                content: error_report.clone(),
                level: "error".to_string(),
            },
        );
        emit_feedback_event(
            &feedback_tx,
            StreamEvent::AgentStatusChanged {
                agent_id,
                label: agent_label,
                status: "error".to_string(),
            },
        );
        return AgentTurnResult {
            report: error_report,
            usage,
            cancelled: false,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    }
    // receipt 落盘后的 Session 更新、第二次控制检查与 active 注销必须共享同一个
    // team 临界区。Cancel 同样在这把锁内持久化 tombstone 并置位：谁先取得锁，
    // 谁就成为本条投递的线性化结果，不会同时提交“成功”和“取消”。
    let (shutdown_after_persist, cancelled_after_persist) = {
        let mut locked_team = team.lock().unwrap_or_else(|poison| poison.into_inner());
        locked_team.registry.set_session(&agent_id, child_session);
        let shutdown = child_shutdown_flag.load(Ordering::Acquire);
        let cancelled = child_cancel_flag.load(Ordering::Acquire);
        locked_team
            .registry
            .update_status(&agent_id, AgentStatus::Idle);
        locked_team.unregister_active_agent(&agent_id);
        (shutdown, cancelled)
    };
    // Shutdown 保留 entry，显式取消丢弃 entry；两者都不返回 completed id。
    if shutdown_after_persist {
        return AgentTurnResult {
            report: format!("[{agent_label}] 会话关闭，已完成投递保留待下次确认"),
            usage,
            cancelled: true,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    }
    if cancelled_after_persist {
        return AgentTurnResult {
            report: format!("[{agent_label}] 本轮已取消"),
            usage,
            cancelled: true,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    }

    active_cleanup.disarm();
    emit_feedback_event(
        &feedback_tx,
        StreamEvent::AgentStatusChanged {
            agent_id: agent_id.clone(),
            label: agent_label,
            status: status.to_string(),
        },
    );

    AgentTurnResult {
        report,
        usage,
        cancelled: child_cancel_flag.load(Ordering::Acquire)
            || child_shutdown_flag.load(Ordering::Acquire),
        completed_delivery_ids: completed_delivery_id.into_iter().collect(),
        main_messages,
        made_progress: true,
    }
}

/// 并发运行多个子 Agent（broadcast 场景），await 全部完成后汇总汇报。
///
/// 每个子 Agent 独立 `run_agent_turn`，用 `FuturesUnordered` 并发驱动。汇总各 Agent
/// 的汇报合并为单条文本，usage 累加。
#[allow(clippy::too_many_arguments)]
pub async fn run_agents_turns(
    team: Arc<Mutex<TeamContext>>,
    agent_ids: Vec<String>,
    storage_root: PathBuf,
    runtime_engine: tiangong_core::runtime::RuntimeEngine,
    parent_tools: Vec<ToolSpec>,
    feedback_tx: PluginFeedbackTx,
    prompt_config: Arc<PromptConfig>,
    execution_semaphore: Arc<tokio::sync::Semaphore>,
    token_budget: Arc<SubAgentTokenBudget>,
) -> AgentTurnResult {
    use futures_util::stream::{FuturesUnordered, StreamExt};

    // 正常来源已由团队注册上限保证最多 8 个目标；这里再次收紧公共执行边界，
    // 防止未来调用方绕过注册约束后把聚合提交放大。
    let agent_ids = agent_ids.into_iter().take(MAX_AGENTS).collect::<Vec<_>>();

    if agent_ids.is_empty() {
        return AgentTurnResult {
            report: "无目标 Agent".to_string(),
            usage: TokenUsage::default(),
            cancelled: false,
            completed_delivery_ids: Vec::new(),
            main_messages: Vec::new(),
            made_progress: false,
        };
    }

    let mut futures: FuturesUnordered<_> = agent_ids
        .into_iter()
        .map(|agent_id| {
            let team = Arc::clone(&team);
            let tools = parent_tools.clone();
            let fb = feedback_tx.clone();
            let pc = Arc::clone(&prompt_config);
            let storage_root = storage_root.clone();
            let execution_semaphore = Arc::clone(&execution_semaphore);
            let token_budget = Arc::clone(&token_budget);
            Box::pin(run_agent_turn(
                team,
                agent_id,
                storage_root,
                runtime_engine.clone(),
                tools,
                fb,
                pc,
                execution_semaphore,
                token_budget,
            ))
                as std::pin::Pin<Box<dyn std::future::Future<Output = AgentTurnResult> + Send>>
        })
        .collect();

    let mut reports = Vec::new();
    let mut total_usage = TokenUsage::default();
    let mut any_cancelled = false;
    let mut any_progress = false;
    let mut completed_delivery_ids = Vec::new();
    let mut main_messages = Vec::new();
    while let Some(result) = futures.next().await {
        if !result.report.trim().is_empty() {
            reports.push(result.report);
        }
        total_usage.accumulate(&result.usage);
        any_cancelled |= result.cancelled;
        any_progress |= result.made_progress;
        completed_delivery_ids.extend(result.completed_delivery_ids);
        main_messages.extend(result.main_messages);
    }

    AgentTurnResult {
        report: reports.join("\n\n"),
        usage: total_usage,
        cancelled: any_cancelled,
        completed_delivery_ids,
        main_messages,
        made_progress: any_progress,
    }
}

/// 同步 Main Agent 提交当前批次后，用原目标列表找出仍有收件箱积压的 Agent。
///
/// 调用方应在父 Core ACK、插件永久结算和 outbox 清理完成后，把返回值交给
/// `schedule_pending_agents`，从而让超出本批边界的工作继续进入下一次安全提交。
pub(crate) fn pending_agent_ids_for_scheduler(
    team: &Arc<Mutex<TeamContext>>,
    candidate_agent_ids: &[String],
) -> Vec<String> {
    let Ok(team) = team.lock() else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    candidate_agent_ids
        .iter()
        .filter(|agent_id| seen.insert(agent_id.as_str()))
        .filter(|agent_id| team.registry.has_pending_inbox_for(agent_id))
        .cloned()
        .collect()
}

/// 渲染当前团队花名册（仅存活 Agent），用于子 Agent system prompt 上下文。
fn format_team_roster(team: &TeamContext) -> String {
    let mut agents = team.registry.alive_agents();
    agents.sort_by(|a, b| a.role.cmp(&b.role));
    agents
        .iter()
        .map(|a| format!("- {} (@{})", a.label, a.role))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 从父工具集中过滤出子 Agent 可用的工具（排除团队管理工具）。
fn filter_sub_agent_tools(
    parent_tools: Vec<ToolSpec>,
    agent_id: &str,
    team: &Arc<Mutex<TeamContext>>,
) -> Vec<ToolSpec> {
    let tool_names: Vec<String> = team
        .lock()
        .ok()
        .and_then(|t| t.registry.get(agent_id).map(|d| d.tools.clone()))
        .unwrap_or_default();
    parent_tools
        .into_iter()
        .filter(|t| tool_names.iter().any(|n| n == &t.name))
        .filter(|t| !matches!(t.name.as_str(), "create_agent" | "dismiss_agent"))
        .collect()
}

/// 构建子 Agent 汇报内容（总结阶段输出优先，回退到所有 Assistant 消息）。
fn build_agent_report(
    child_session: &Session,
    start_message_len: usize,
    agent_label: &str,
) -> String {
    let new_messages = child_session
        .messages
        .get(start_message_len..)
        .unwrap_or_default();
    let summary = {
        let summaries: Vec<String> = new_messages
            .iter()
            .filter(|m| {
                m.role == MessageRole::Assistant
                    && m.phase == tiangong_core::session::MessagePhase::Summary
            })
            .filter_map(|m| {
                let c = m.text_content().trim().to_string();
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        if summaries.is_empty() {
            new_messages
                .iter()
                .filter(|m| m.role == MessageRole::Assistant)
                .filter_map(|m| {
                    let c = m.text_content().trim().to_string();
                    if c.is_empty() {
                        None
                    } else {
                        Some(c)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            summaries.join("\n")
        }
    };

    if summary.is_empty() {
        format!("[{agent_label}] 已完成本轮工作，但没有生成文本输出。")
    } else {
        let brief = if summary.chars().count() > 500 {
            format!("{}...", summary.chars().take(500).collect::<String>())
        } else {
            summary
        };
        format!("[{agent_label}] 执行完成\n{brief}")
    }
}

/// 启动转发线程：把子 Agent 的内部 StreamEvent 翻译为父级事件经 feedback 投递。
///
/// 子 Agent 的细粒度事件（Delta/Reasoning/ToolCalls/ToolStart/ToolResult/...）
/// 收敛为 `AgentOutput`（带 agent_id/role/label），GUI 的 AgentPanel 据此把输出
/// 归因到具体 Agent Tab。团队级事件（AgentCreated/StatusChanged/...）透传。
/// TokenUsage 标记子 Agent 归属并即时扣减团队预算，父 turn 用量仍只聚合一次；Done 抑制。
#[allow(clippy::too_many_arguments)]
fn spawn_sub_agent_stream_forwarder(
    agent_id: String,
    agent_role: String,
    agent_label: String,
    feedback_tx: PluginFeedbackTx,
    team: Arc<Mutex<TeamContext>>,
    token_budget: Arc<SubAgentTokenBudget>,
    observed_usage: ObservedSubAgentUsage,
    child_rx: std::sync::mpsc::Receiver<StreamEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut turn_observed_usage = TokenUsage::default();
        for event in child_rx {
            match event {
                StreamEvent::Delta {
                    message_id,
                    content,
                }
                | StreamEvent::ReactText {
                    message_id,
                    content,
                }
                | StreamEvent::SummaryText {
                    message_id,
                    content,
                } => send_sub_agent_output(
                    &feedback_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:assistant:{message_id}"),
                        MessageRole::Assistant,
                        content,
                        "",
                    ),
                ),
                StreamEvent::Reasoning {
                    message_id,
                    content,
                } => send_sub_agent_output(
                    &feedback_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:assistant:{message_id}"),
                        MessageRole::Assistant,
                        "",
                        content,
                    ),
                ),
                StreamEvent::ToolCalls {
                    message_id,
                    names,
                    usage,
                    ..
                } => {
                    let usage_text = usage
                        .map(|u| {
                            format!(
                                "\ntokens: prompt={}, completion={}, total={}",
                                u.prompt_tokens, u.completion_tokens, u.total_tokens
                            )
                        })
                        .unwrap_or_default();
                    send_sub_agent_output(
                        &feedback_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            format!("agent:{agent_id}:tool-calls:{message_id}"),
                            MessageRole::System,
                            format!("LLM 输出{usage_text}\ntool_calls: {}", names.join(", ")),
                            "",
                        ),
                    );
                }
                StreamEvent::ToolStart { name, args_summary } => {
                    let mut content = format!("工具开始 [{name}]");
                    if !args_summary.is_empty() {
                        content.push_str(&format!("\n命令: {args_summary}"));
                    }
                    send_sub_agent_output(
                        &feedback_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            format!("agent:{agent_id}:tool-start:{}", scru128::new()),
                            MessageRole::System,
                            content,
                            "",
                        ),
                    );
                }
                StreamEvent::ToolResult {
                    name,
                    tool_call_id,
                    ok,
                    output,
                    full_output,
                    ..
                } => {
                    let persisted = full_output.unwrap_or(output);
                    let mut content = format!(
                        "工具执行 [{name}]\nok={} exit_code={}",
                        ok,
                        if ok { 0 } else { 1 }
                    );
                    if !persisted.trim().is_empty() {
                        content.push_str(&format!("\nstdout:\n{persisted}"));
                    }
                    send_sub_agent_output(
                        &feedback_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            tool_call_id.unwrap_or_else(|| {
                                format!("agent:{agent_id}:tool-result:{}", scru128::new())
                            }),
                            MessageRole::System,
                            content,
                            "",
                        ),
                    );
                }
                StreamEvent::TokenUsage {
                    usage,
                    current_tokens,
                    compression_threshold_tokens,
                    context_limit_tokens,
                    source,
                    ..
                } => {
                    let cumulative = source == "cancelled-cumulative";
                    let newly_observed = observed_usage.record_stream_event(
                        &usage,
                        cumulative,
                        &mut turn_observed_usage,
                    );
                    if newly_observed.total_tokens > 0 {
                        let budget_charge = token_budget.record_usage(newly_observed.total_tokens);
                        pause_team_for_budget(&team, &agent_id, &feedback_tx, budget_charge);
                    }
                    if newly_observed.total_tokens > 0 {
                        emit_feedback_event(
                            &feedback_tx,
                            StreamEvent::TokenUsage {
                                usage: newly_observed,
                                current_tokens,
                                compression_threshold_tokens,
                                context_limit_tokens,
                                // child 的累计取消快照已在转发边界换算为增量，Core 不应
                                // 再按累计事件去重。
                                source: if cumulative {
                                    "sub_agent_cancelled_delta".to_string()
                                } else {
                                    source
                                },
                                agent_id: Some(agent_id.clone()),
                            },
                        );
                    }
                }
                StreamEvent::AgentCreated { .. }
                | StreamEvent::AgentStatusChanged { .. }
                | StreamEvent::AgentNotification { .. }
                | StreamEvent::AgentMessage { .. }
                | StreamEvent::FileLockChanged { .. }
                | StreamEvent::PhaseChanged { .. }
                | StreamEvent::ApprovalNeeded { .. }
                | StreamEvent::Retry { .. } => {
                    emit_feedback_event(&feedback_tx, event);
                }
                StreamEvent::Error { message } => {
                    emit_feedback_event(
                        &feedback_tx,
                        StreamEvent::AgentNotification {
                            agent_id: agent_id.clone(),
                            agent_label: agent_label.clone(),
                            content: format!("执行出错：{message}"),
                            level: "error".to_string(),
                        },
                    );
                }
                _ => {}
            }
        }
    })
}

/// 构造一条子 Agent 流转发的 Message。
fn sub_agent_stream_message(
    id: impl Into<String>,
    role: MessageRole,
    content: impl Into<String>,
    reasoning_content: impl Into<String>,
) -> tiangong_types::Message {
    let mut message = tiangong_types::Message::with_reasoning(role, content, reasoning_content);
    message.id = id.into();
    message
}

/// 把一条 Message 包装为 StreamEvent::AgentOutput 经 feedback 推送。
fn send_sub_agent_output(
    feedback_tx: &PluginFeedbackTx,
    agent_id: &str,
    agent_role: &str,
    agent_label: &str,
    message: tiangong_types::Message,
) {
    emit_feedback_event(
        feedback_tx,
        StreamEvent::AgentOutput {
            agent_id: agent_id.to_string(),
            agent_role: agent_role.to_string(),
            agent_label: agent_label.to_string(),
            messages: vec![message],
        },
    );
}

fn emit_feedback_event(feedback_tx: &PluginFeedbackTx, event: StreamEvent) {
    if !feedback_tx.send_turn_stream_event(event.clone()) {
        feedback_tx.send_stream_event(event);
    }
}

#[cfg(test)]
mod tests {
    use crate::state::MessagePriority;

    use super::*;

    fn token_usage(prompt_tokens: usize, completion_tokens: usize) -> TokenUsage {
        TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }
    }

    #[test]
    fn oversized_receipt_backlog_stops_before_core_commit_limit_and_keeps_remainder() {
        let agent_id = "dev-agent";
        let backlog_len = MAX_PLUGIN_DELIVERIES_PER_COMMIT + 1;
        let mut child_session = Session::new("child");
        for index in 0..backlog_len {
            append_delivery_receipt(
                &mut child_session,
                &format!("delivery-{index}"),
                &format!("report-{index}"),
                &[],
            )
            .unwrap();
        }

        let mut team = TeamContext::new();
        team.registry.register_with_session(
            crate::AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "work".to_string(),
                tools: Vec::new(),
                status: AgentStatus::Idle,
            },
            child_session,
        );
        for index in 0..backlog_len {
            team.registry.deliver_message(
                agent_id,
                AgentMessage {
                    id: format!("delivery-{index}"),
                    from: "main".to_string(),
                    to: agent_id.to_string(),
                    content: format!("work-{index}"),
                    priority: MessagePriority::Normal,
                    created_at: "2026-07-12 12:00:00".to_string(),
                },
            );
        }

        let mut completed_delivery_ids = Vec::new();
        while agent_delivery_batch_has_capacity(completed_delivery_ids.len()) {
            let entry = team.registry.take_next_inbox_entry(agent_id).unwrap();
            assert!(
                delivery_receipt(
                    team.registry.get_session(agent_id).unwrap(),
                    &entry.message.id,
                )
                .is_some(),
                "测试积压全部走零 token 的持久回执重放路径"
            );
            completed_delivery_ids.push(entry.message.id);
        }

        assert_eq!(completed_delivery_ids.len(), MAX_AGENT_DELIVERIES_PER_TURN);
        assert_eq!(
            completed_delivery_ids.len() * MAX_AGENTS,
            MAX_PLUGIN_DELIVERIES_PER_COMMIT
        );
        assert!(team.registry.has_pending_inbox_for(agent_id));

        let team = Arc::new(Mutex::new(team));
        assert_eq!(
            pending_agent_ids_for_scheduler(&team, &[agent_id.to_string()]),
            [agent_id]
        );
        let mut remaining = 0;
        while team
            .lock()
            .unwrap()
            .registry
            .take_next_inbox_entry(agent_id)
            .is_some()
        {
            remaining += 1;
        }
        assert_eq!(remaining, backlog_len - MAX_AGENT_DELIVERIES_PER_TURN);
    }

    #[test]
    fn concurrent_budget_charges_saturate_once_and_reset_for_next_wave() {
        let budget = Arc::new(SubAgentTokenBudget::new());
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let budget = Arc::clone(&budget);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    budget.record_usage(50_000)
                })
            })
            .collect::<Vec<_>>();
        let charges = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(budget.used_tokens(), SUB_AGENT_TOTAL_TOKEN_BUDGET);
        assert!(budget.is_paused());
        assert_eq!(
            charges
                .iter()
                .filter(|charge| charge.newly_exhausted)
                .count(),
            1
        );

        assert!(budget.reset());
        assert_eq!(budget.used_tokens(), 0);
        assert!(!budget.is_paused());
    }

    #[test]
    fn interrupted_sub_agent_recovers_observed_usage_once() {
        let mut recovery = SubAgentUsageRecoveryState::new();
        let observer = recovery.observer();
        observer.record(&token_usage(2, 3));
        observer.record(&token_usage(5, 7));

        let (usage, unaccounted_tokens) = recovery.take_for_recovery();
        assert_eq!(usage.prompt_tokens, 7);
        assert_eq!(usage.completion_tokens, 10);
        assert_eq!(usage.total_tokens, 17);
        assert_eq!(unaccounted_tokens, 17);

        let (duplicate, duplicate_unaccounted) = recovery.take_for_recovery();
        assert_eq!(duplicate.total_tokens, 0);
        assert_eq!(duplicate_unaccounted, 0);
    }

    #[test]
    fn stream_usage_is_budgeted_incrementally_without_double_counting_cancel_total() {
        let mut recovery = SubAgentUsageRecoveryState::new();
        let observer = recovery.observer();
        let budget = SubAgentTokenBudget::new();
        let mut turn_observed = TokenUsage::default();

        let first_delta =
            observer.record_stream_event(&token_usage(100_000, 50_000), false, &mut turn_observed);
        assert_eq!(first_delta.total_tokens, 150_000);
        assert!(!budget.record_usage(first_delta.total_tokens).exhausted);

        // 取消事件携带本轮累计值，只应补记相对前一个流事件的增量。
        let cancellation_delta =
            observer.record_stream_event(&token_usage(125_000, 75_000), true, &mut turn_observed);
        assert_eq!(cancellation_delta.total_tokens, 50_000);
        let charge = budget.record_usage(cancellation_delta.total_tokens);
        assert!(charge.exhausted);
        assert!(charge.newly_exhausted);
        assert_eq!(budget.used_tokens(), SUB_AGENT_TOTAL_TOKEN_BUDGET);

        let (usage, unaccounted_tokens) = recovery.take_for_recovery();
        assert_eq!(usage.total_tokens, 200_000);
        assert_eq!(unaccounted_tokens, 0);
    }

    #[test]
    fn consecutive_cancelled_turns_use_independent_cumulative_baselines() {
        let observer = ObservedSubAgentUsage::default();
        let mut first_turn = TokenUsage::default();
        let mut second_turn = TokenUsage::default();

        let first = observer.record_stream_event(&token_usage(10, 5), true, &mut first_turn);
        let second = observer.record_stream_event(&token_usage(7, 3), true, &mut second_turn);

        assert_eq!(first.total_tokens, 15);
        assert_eq!(second.total_tokens, 10);
        assert_eq!(observer.total_tokens(), 25);
    }

    #[test]
    fn normally_completed_sub_agent_does_not_recover_observed_usage() {
        let mut recovery = SubAgentUsageRecoveryState::new();
        recovery.observer().record(&token_usage(11, 13));

        recovery.disarm();

        let (usage, unaccounted_tokens) = recovery.take_for_recovery();
        assert_eq!(usage.total_tokens, 0);
        assert_eq!(unaccounted_tokens, 0);
    }

    #[test]
    fn delivery_receipt_round_trips_report_and_main_messages() {
        let mut session = Session::new("child");
        let main_message = AgentMessage {
            id: "main-message-1".to_string(),
            from: "dev-agent".to_string(),
            to: "main".to_string(),
            content: "实现已完成".to_string(),
            priority: MessagePriority::Normal,
            created_at: "2026-07-12 12:00:00".to_string(),
        };

        append_delivery_receipt(
            &mut session,
            "delivery-1",
            "[Developer] 执行完成",
            std::slice::from_ref(&main_message),
        )
        .unwrap();

        let persisted = session.messages.last().unwrap();
        assert!(persisted.model_excluded);
        let receipt = delivery_receipt(&session, "delivery-1").unwrap();
        assert_eq!(receipt.report, "[Developer] 执行完成");
        assert_eq!(receipt.main_messages.len(), 1);
        assert_eq!(receipt.main_messages[0].id, main_message.id);
        assert!(delivery_receipt(&session, "delivery-other").is_none());
    }

    #[test]
    fn unacknowledged_outbox_survives_replay_and_is_removed_only_after_ack() {
        let mut session = Session::new("child");
        let receipted_message = AgentMessage {
            id: "main-message-receipted".to_string(),
            from: "dev-agent".to_string(),
            to: "main".to_string(),
            content: "第一次汇报".to_string(),
            priority: MessagePriority::Normal,
            created_at: "2026-07-12 12:00:00".to_string(),
        };
        append_delivery_receipt(
            &mut session,
            "ordinary-work-1",
            "第一次工作完成",
            std::slice::from_ref(&receipted_message),
        )
        .unwrap();

        let new_message = AgentMessage {
            id: "main-message-new".to_string(),
            content: "第二次汇报".to_string(),
            ..receipted_message.clone()
        };
        let other_agent_message = AgentMessage {
            id: "main-message-other-agent".to_string(),
            from: "test-agent".to_string(),
            content: "测试汇报".to_string(),
            ..receipted_message.clone()
        };
        let team = Arc::new(Mutex::new(TeamContext::new()));
        {
            let mut locked = team.lock().unwrap();
            let replay = delivery_receipt(&session, "ordinary-work-1").unwrap();
            restore_receipt_main_messages(&mut locked, &replay.main_messages);
            restore_receipt_main_messages(&mut locked, &replay.main_messages);
            assert_eq!(locked.main_inbox.len(), 1, "回执重放必须按消息 ID 去重");
            let (command_tx, _command_rx) = tokio::sync::mpsc::unbounded_channel();
            locked.register_active_agent(
                "dev-agent".to_string(),
                command_tx,
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
                Some("ordinary-work-2".to_string()),
            );
            locked.deliver_main_message(new_message.clone());
            locked.main_inbox.push(other_agent_message.clone());
            let unreceipted =
                collect_unreceipted_main_messages(&locked, &session, "ordinary-work-2");
            assert_eq!(unreceipted.len(), 1);
            assert_eq!(unreceipted[0].id, new_message.id);
            assert_eq!(locked.main_inbox.len(), 3, "父 Core ACK 前不能删除 outbox");
        }

        let replay = delivery_receipt(&session, "ordinary-work-1").unwrap();
        assert_eq!(replay.main_messages[0].id, receipted_message.id);
        assert_eq!(
            remove_acknowledged_main_messages(
                &team,
                &[receipted_message.id.clone(), new_message.id.clone()],
            ),
            2
        );
        let remaining = team.lock().unwrap();
        assert_eq!(remaining.main_inbox.len(), 1);
        assert_eq!(remaining.main_inbox[0].id, other_agent_message.id);
    }

    fn pending_entry(work_id: &str) -> crate::state::message_bus::AgentInboxEntry {
        crate::state::message_bus::AgentInboxEntry {
            message: AgentMessage {
                id: work_id.to_string(),
                from: "main".to_string(),
                to: "dev-agent".to_string(),
                content: "work".to_string(),
                priority: MessagePriority::Normal,
                created_at: "2026-07-12 12:00:00".to_string(),
            },
            additional_content: Vec::new(),
            session_message_id: None,
        }
    }

    fn main_message_for_work(message_id: &str) -> AgentMessage {
        AgentMessage {
            id: message_id.to_string(),
            from: "dev-agent".to_string(),
            to: "main".to_string(),
            content: "progress".to_string(),
            priority: MessagePriority::Normal,
            created_at: "2026-07-12 12:00:00".to_string(),
        }
    }

    fn team_with_active_work(work_id: &str) -> TeamContext {
        let mut team = team_with_child_session(Session::new("child"));
        let (command_tx, _command_rx) = tokio::sync::mpsc::unbounded_channel();
        team.register_active_agent(
            "dev-agent".to_string(),
            command_tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Some(work_id.to_string()),
        );
        team.registry
            .update_status("dev-agent", AgentStatus::Running);
        team
    }

    #[test]
    fn cancelled_and_failed_attempts_remove_their_uncommitted_main_messages() {
        for (cancelled, should_requeue) in [(true, false), (false, true)] {
            let mut team = team_with_active_work("work-1");
            team.deliver_main_message(main_message_for_work("main-attempt-1"));
            let mut entry = Some(pending_entry("work-1"));

            cleanup_active_attempt(&mut team, "dev-agent", &mut entry, cancelled, false);

            assert!(team.main_inbox.is_empty(), "失败尝试的 main 消息不得迟到");
            assert_eq!(
                team.registry.has_pending_inbox_for("dev-agent"),
                should_requeue
            );
        }
    }

    #[test]
    fn shutdown_retry_starts_without_duplicate_main_messages() {
        let mut team = team_with_active_work("work-1");
        team.deliver_main_message(main_message_for_work("main-stable"));
        let mut entry = Some(pending_entry("work-1"));

        cleanup_active_attempt(&mut team, "dev-agent", &mut entry, true, true);
        assert!(team.main_inbox.is_empty());
        let retried = team
            .registry
            .take_next_inbox_entry("dev-agent")
            .expect("shutdown 必须把稳定工作放回队首");
        assert_eq!(retried.message.id, "work-1");

        let (command_tx, _command_rx) = tokio::sync::mpsc::unbounded_channel();
        team.register_active_agent(
            "dev-agent".to_string(),
            command_tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Some("work-1".to_string()),
        );
        team.deliver_main_message(main_message_for_work("main-stable"));
        team.deliver_main_message(main_message_for_work("main-stable"));
        assert_eq!(team.main_inbox.len(), 1);
        assert_eq!(team.main_messages_for_work("work-1").len(), 1);
    }

    fn team_with_child_session(session: Session) -> TeamContext {
        let mut team = TeamContext::new();
        team.registry.register_with_session(
            crate::AgentDescriptor {
                agent_id: "dev-agent".to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "Implement".to_string(),
                tools: Vec::new(),
                status: AgentStatus::Idle,
            },
            session,
        );
        team
    }

    #[test]
    fn receipt_scan_recovers_ordinary_work_without_inbox() {
        let mut session = Session::new("child");
        append_delivery_receipt(&mut session, "ordinary-work", "完成", &[]).unwrap();
        append_delivery_receipt(&mut session, "completed-work", "已确认", &[]).unwrap();
        append_delivery_receipt(&mut session, "cancelled-work", "已取消", &[]).unwrap();
        append_delivery_receipt(&mut session, "settled-work", "已永久结算", &[]).unwrap();
        let team = team_with_child_session(session);

        let replays = pending_receipt_replays(
            &team,
            &["completed-work".to_string()],
            &["cancelled-work".to_string()],
            &["settled-work".to_string()],
        );
        assert_eq!(replays.len(), 1);
        assert_eq!(replays[0].agent_id, "dev-agent");
        assert_eq!(replays[0].delivery_id, "ordinary-work");
        assert_eq!(replays[0].report, "完成");
        assert!(replays[0].main_messages.is_empty());
    }

    #[test]
    fn legacy_persisted_ack_marker_remains_compatible_after_parent_ledger_eviction() {
        let mut session = Session::new("child");
        session.parent_session_id = Some("parent-session".to_string());
        append_delivery_receipt(&mut session, "ordinary-work", "完成", &[]).unwrap();
        let payload = serde_json::to_string(&DeliveryReceiptAck {
            delivery_ids: vec!["ordinary-work".to_string()],
        })
        .unwrap();
        let mut marker = Message::new(
            MessageRole::System,
            format!("{DELIVERY_RECEIPT_ACK_MARKER}{payload}"),
        );
        marker.model_excluded = true;
        session.messages.push(marker);
        let storage = tempfile::tempdir().unwrap();
        crate::lifecycle::persist_child_session_for_parent_id(
            storage.path(),
            "parent-session",
            "dev-agent",
            &session,
        )
        .unwrap();
        let team = team_with_child_session(session);
        assert!(pending_receipt_replays(&team, &[], &[], &[]).is_empty());

        let mut parent = Session::new("parent");
        parent.id = "parent-session".to_string();
        let restored = crate::lifecycle::load_child_session(storage.path(), &parent, "dev-agent")
            .expect("ACK marker should be persisted with child session");
        let restored_team = team_with_child_session(restored);
        assert!(pending_receipt_replays(&restored_team, &[], &[], &[]).is_empty());
    }
}
