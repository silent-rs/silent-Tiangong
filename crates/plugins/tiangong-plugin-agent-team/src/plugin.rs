//! Agent Team 插件结构体与生命周期实现。
//!
//! [`AgentTeamPlugin`] 经以下方式获取运行时上下文：
//! - **构造**：注入宿主存储根，并创建进程级共享、跨子引擎 clone 的 `TeamContext`。
//! - [`Plugin::register`]：捕获 `RuntimeEngine` clone，供子 Agent `execute_turn`
//!   构造子 ReactEngine 用。
//! - [`Plugin::set_feedback_tx`]：注入状态反馈通道（转发流事件、上报 usage、注入汇报）。
//! - 消息投递钩子：在 Core 落盘前规划用户 @提及，落盘后串行调度目标 Agent。
//! - [`Plugin::on_engine_rebuilt`] / [`Plugin::on_session_ready`]：从会话历史恢复 Agent。
//!
//! ## 实例模型
//!
//! `AgentTeamPlugin` 实例是 **per-Core** 的（每次 engine 创建现场构造）。`team`
//! 持有的 `TeamContext` 也随之 per-Core——同一时刻一个 Core 一个 session 一个团队。
//! `runtime_engine` 在 `register` 时 clone 捕获（与父引擎共享同一 `tool_overrides`）。
//!
//! ## 文件锁
//!
//! 文件锁（`lock_file` / `unlock_file`）由本插件内部管理，并通过插件声明的实际写入
//! 目标强制检查所有子 Agent 文件修改。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde_json::json;
use tiangong_core::core::command::Command;
use tiangong_core::core::plugin::{
    PluginFeedback, PluginFeedbackTx, MAX_PLUGIN_DELIVERIES_PER_COMMIT,
};
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::prompt::SystemPromptConfig;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::session::{PendingPluginDelivery, Session};
use tiangong_core::tool_override::PromptSectionProvider;
use tiangong_types::ContentBlock;

use crate::cancellation::CancellationTombstoneStore;
use crate::constants::MAX_CONCURRENT_SUB_AGENTS;
use crate::lifecycle::{
    dispatch_pending_agent_deliveries, explicitly_terminated_agent_ids,
    plan_user_mention_deliveries, restore_agents_from_session_history,
    restore_pending_agent_deliveries,
};
use crate::team_bridge::{PromptConfig, SubAgentTokenBudget};
use crate::TeamContext;

/// 当前调用方身份（默认 "main"，子 Agent turn 内由其引擎 agent_id 决定）。
pub(crate) const MAIN_AGENT_ID: &str = "main";

/// Agent Team 插件。
pub struct AgentTeamPlugin {
    /// 宿主注入的存储根，child session 持久化不得读取 Core 私有全局状态。
    storage_root: PathBuf,
    /// 显式取消先落入插件自管 tombstone，父 Session ACK 后再删除。
    cancellation_store: CancellationTombstoneStore,
    /// 团队上下文（进程级共享，跨子引擎 clone）。
    pub team: Arc<Mutex<TeamContext>>,
    /// 状态反馈通道（转发子 Agent 流事件、上报 usage、注入汇报）。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
    /// 父 RuntimeEngine clone（register 时捕获，供子 Agent 构造子 ReactEngine）。
    runtime_engine: RwLock<Option<RuntimeEngine>>,
    /// 父工具快照（register 时捕获，供子 Agent 过滤可用工具）。
    parent_tools: RwLock<Vec<ToolSpec>>,
    /// 当前会话 id（on_session_ready 设置，供 child session 持久化）。
    session_id: RwLock<Option<String>>,
    /// PromptConfig（on_engine_rebuilt 构建，供子 Agent system prompt）。
    prompt_config: RwLock<Option<Arc<PromptConfig>>>,
    /// 当前会话工作目录，文件锁与实际写入使用同一规范化路径。
    workspace: RwLock<Option<PathBuf>>,
    /// 所有子 Agent 共用的并发上限。
    execution_semaphore: Arc<tokio::sync::Semaphore>,
    /// 当前连续执行波次累计的子 Agent token 消耗；团队空闲后开始新波次时重置。
    sub_agent_token_budget: Arc<SubAgentTokenBudget>,
    /// 新主 turn 到来时旧波次仍在收敛，则记录一次“空闲后恢复”请求。
    ///
    /// 该门控只由主 turn 置位、最后一个调度 worker 消费，禁止同一波在没有新主
    /// turn 的情况下因自然空闲而自行重置预算。
    budget_resume_requested: Arc<AtomicBool>,
    /// 会话关闭后禁止再接受新的后台调度。
    stopping: Arc<AtomicBool>,
    /// 串行化“父 Session 提交 → 插件永久结算”，任何时刻最多存在一笔未结算提交。
    delivery_commit_gate: Arc<tokio::sync::Mutex<()>>,
    /// 后台直达任务，供 shutdown 形成真实等待栅栏。
    background_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

#[derive(Clone)]
pub(crate) struct AgentSchedulerContext {
    team: Arc<Mutex<TeamContext>>,
    storage_root: PathBuf,
    runtime_engine: RuntimeEngine,
    parent_tools: Vec<ToolSpec>,
    feedback_tx: PluginFeedbackTx,
    prompt_config: Arc<PromptConfig>,
    execution_semaphore: Arc<tokio::sync::Semaphore>,
    token_budget: Arc<SubAgentTokenBudget>,
    budget_resume_requested: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    cancellation_store: CancellationTombstoneStore,
    delivery_commit_gate: Arc<tokio::sync::Mutex<()>>,
    background_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

struct ReceiptReplayCommitBatch {
    delivery_ids: Vec<String>,
    main_message_ids: Vec<String>,
    injection: PluginFeedback,
}

fn delivery_id_commit_batches(delivery_ids: Vec<String>) -> Vec<Vec<String>> {
    delivery_ids
        .chunks(MAX_PLUGIN_DELIVERIES_PER_COMMIT)
        .map(<[String]>::to_vec)
        .collect()
}

/// 每个恢复批次独立构造报告与注入，禁止后续小批次复用首批的全量 payload。
fn receipt_replay_commit_batches(
    replays: Vec<crate::team_bridge::PendingReceiptReplay>,
) -> Vec<ReceiptReplayCommitBatch> {
    replays
        .chunks(MAX_PLUGIN_DELIVERIES_PER_COMMIT)
        .map(|batch| {
            let delivery_ids = batch
                .iter()
                .map(|replay| replay.delivery_id.clone())
                .collect::<Vec<_>>();
            let main_message_ids = batch
                .iter()
                .flat_map(|replay| replay.main_messages.iter())
                .map(|message| message.id.clone())
                .collect::<Vec<_>>();
            let reports = batch
                .iter()
                .map(|replay| {
                    json!({
                        "agent_id": replay.agent_id.clone(),
                        "delivery_id": replay.delivery_id.clone(),
                        "report": replay.report.clone(),
                        "messages": replay.main_messages.clone(),
                    })
                })
                .collect::<Vec<_>>();
            let injection = PluginFeedback::new(
                "agent_team_recovered_reports",
                json!({ "reports": reports, "delivery_ids": delivery_ids.clone() }),
            );
            ReceiptReplayCommitBatch {
                delivery_ids,
                main_message_ids,
                injection,
            }
        })
        .collect()
}

fn spawn_registered_task_inner<F>(
    stopping: &AtomicBool,
    background_tasks: &Mutex<Vec<tokio::task::JoinHandle<()>>>,
    runtime_handle: &tokio::runtime::Handle,
    future: F,
) -> bool
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let Ok(mut tasks) = background_tasks.lock() else {
        return false;
    };
    if stopping.load(Ordering::Acquire) {
        return false;
    }
    tasks.retain(|task| !task.is_finished());
    tasks.push(runtime_handle.spawn(future));
    true
}

fn spawn_registered_finalizer_inner<F>(
    stopping: &AtomicBool,
    background_tasks: &Mutex<Vec<tokio::task::JoinHandle<()>>>,
    runtime_handle: &tokio::runtime::Handle,
    commit_guard: tokio::sync::OwnedMutexGuard<()>,
    finalizer: F,
) -> Result<tokio::sync::oneshot::Receiver<bool>, String>
where
    F: std::future::Future<Output = bool> + Send + 'static,
{
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let task = async move {
        // guard 随登记任务而不是调用方 Future 生存；调用方取消不能放行下一笔提交。
        let _commit_guard = commit_guard;
        let settled = finalizer.await;
        let _ = result_tx.send(settled);
    };
    if spawn_registered_task_inner(stopping, background_tasks, runtime_handle, task) {
        Ok(result_rx)
    } else if stopping.load(Ordering::Acquire) {
        Err("Agent Team 正在关闭，无法登记投递结算任务".to_string())
    } else {
        Err("Agent Team 后台任务队列不可用，无法登记投递结算任务".to_string())
    }
}

/// 清除一个 worker 的调度占用，并在它确实是最后一个 worker 时消费主 turn 恢复请求。
///
/// 返回值中的 Agent 列表由调用方继续调度。关闭阶段不消费请求、不重置预算。
fn finish_worker_and_take_budget_resume(
    team: &Mutex<TeamContext>,
    agent_id: &str,
    token_budget: &SubAgentTokenBudget,
    budget_resume_requested: &AtomicBool,
    stopping: &AtomicBool,
) -> (bool, Vec<String>) {
    let Ok(mut team) = team.lock() else {
        return (false, Vec::new());
    };
    team.finish_in_flight(agent_id);
    if stopping.load(Ordering::Acquire)
        || team.has_execution_work()
        || budget_resume_requested
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return (false, Vec::new());
    }
    let was_paused = token_budget.reset();
    (was_paused, team.registry.agent_ids_with_pending_inbox())
}

impl AgentTeamPlugin {
    pub fn new(storage_root: PathBuf) -> Self {
        Self {
            cancellation_store: CancellationTombstoneStore::new(storage_root.clone()),
            storage_root,
            team: Arc::new(Mutex::new(TeamContext::new())),
            feedback_tx: RwLock::new(None),
            runtime_engine: RwLock::new(None),
            parent_tools: RwLock::new(Vec::new()),
            session_id: RwLock::new(None),
            prompt_config: RwLock::new(None),
            workspace: RwLock::new(None),
            execution_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SUB_AGENTS)),
            sub_agent_token_budget: Arc::new(SubAgentTokenBudget::new()),
            budget_resume_requested: Arc::new(AtomicBool::new(false)),
            stopping: Arc::new(AtomicBool::new(false)),
            delivery_commit_gate: Arc::new(tokio::sync::Mutex::new(())),
            background_tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 读取反馈通道的 clone（供 handler 转发流事件用）。
    pub(crate) fn feedback_tx(&self) -> Option<PluginFeedbackTx> {
        self.feedback_tx.read().ok()?.as_ref().cloned()
    }

    /// 父工具快照 clone（供 execute_team_tool 解析子 Agent 可用工具 + 子引擎构造）。
    pub(crate) fn parent_tools_snapshot(&self) -> Vec<ToolSpec> {
        self.parent_tools
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// 父 RuntimeEngine clone 快照（供 handler 构造子 ReactEngine）。
    pub(crate) fn runtime_engine_snapshot(&self) -> Option<RuntimeEngine> {
        self.runtime_engine.read().ok()?.as_ref().cloned()
    }

    /// PromptConfig 快照（供子 Agent system prompt 构建）。
    pub(crate) fn prompt_config_snapshot(&self) -> Option<Arc<PromptConfig>> {
        self.prompt_config.read().ok()?.clone()
    }

    /// 宿主存储根（供恢复 child session）。
    pub(crate) fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// 宿主存储根快照（供异步子 Agent 执行持有）。
    pub(crate) fn storage_root_snapshot(&self) -> PathBuf {
        self.storage_root.clone()
    }

    pub(crate) fn execution_semaphore(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(&self.execution_semaphore)
    }

    pub(crate) fn sub_agent_token_budget(&self) -> Arc<SubAgentTokenBudget> {
        Arc::clone(&self.sub_agent_token_budget)
    }

    pub(crate) fn delivery_protocol_store(&self) -> CancellationTombstoneStore {
        self.cancellation_store.clone()
    }

    pub(crate) fn stopping_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stopping)
    }

    pub(crate) fn delivery_commit_gate(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.delivery_commit_gate)
    }

    /// 在线性化关闭边界内启动并登记后台任务。
    ///
    /// `shutdown` 先置 `stopping`，再取得同一把 `background_tasks` 锁并取走全部
    /// 已登记任务。这里持锁复查 `stopping` 后才 spawn + push，因此任务要么被拒绝，
    /// 要么一定能被关闭流程取走并等待，不会落入两步之间的空窗。
    fn spawn_registered_task<F>(&self, runtime_handle: &tokio::runtime::Handle, future: F) -> bool
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        spawn_registered_task_inner(
            &self.stopping,
            &self.background_tasks,
            runtime_handle,
            future,
        )
    }

    /// 新主会话 turn 开始时，若团队空闲则重置预算并返回需要恢复调度的 Agent。
    fn reset_budget_for_main_turn_if_idle(&self) -> (bool, Vec<String>) {
        let Ok(team) = self.team.lock() else {
            return (false, Vec::new());
        };
        if team.has_execution_work() {
            self.budget_resume_requested.store(true, Ordering::Release);
            return (false, Vec::new());
        }
        self.budget_resume_requested.store(false, Ordering::Release);
        let was_paused = self.sub_agent_token_budget.reset();
        (was_paused, team.registry.agent_ids_with_pending_inbox())
    }

    fn notify_budget_resumed(&self) {
        let Some(feedback) = self.feedback_tx() else {
            return;
        };
        let event = tiangong_types::StreamEvent::AgentNotification {
            agent_id: "agent-team-budget".to_string(),
            agent_label: "Agent Team".to_string(),
            content: "新的主会话轮次已重置 Sub Agent token 预算，暂停任务正在自动恢复。"
                .to_string(),
            level: "info".to_string(),
        };
        if !feedback.send_turn_stream_event(event.clone()) {
            feedback.send_stream_event(event);
        }
    }

    fn current_session_id(&self) -> Result<String, String> {
        self.session_id
            .read()
            .map_err(|_| "Agent Team 会话状态锁定失败".to_string())?
            .clone()
            .ok_or_else(|| "Agent Team 尚未绑定父会话，无法持久化取消记录".to_string())
    }

    pub(crate) fn persist_delivery_cancellation(
        &self,
        delivery_ids: &[String],
    ) -> Result<(), String> {
        if delivery_ids.is_empty() {
            return Ok(());
        }
        let session_id = self.current_session_id()?;
        self.cancellation_store
            .record_cancelled(&session_id, delivery_ids.iter().cloned())
            .map(|_| ())
    }

    fn notify_cancellation_persist_failure(&self, error: &str) {
        let Some(feedback) = self.feedback_tx() else {
            return;
        };
        let event = tiangong_types::StreamEvent::AgentNotification {
            agent_id: "agent-team-cancellation".to_string(),
            agent_label: "Agent Team".to_string(),
            content: format!("Agent 投递协议记录不可用，相关任务已保留且不会冒险丢弃：{error}"),
            level: "error".to_string(),
        };
        if !feedback.send_turn_stream_event(event.clone()) {
            feedback.send_stream_event(event);
        }
    }

    /// 取消 tombstone 已成功落盘后，提交父 Session 完成状态并等待持久化 ACK。
    pub(crate) fn submit_recorded_delivery_cancellation(&self, mut delivery_ids: Vec<String>) {
        delivery_ids.sort();
        delivery_ids.dedup();
        if delivery_ids.is_empty() {
            return;
        }
        let Some(feedback) = self.feedback_tx() else {
            return;
        };
        let delivery_batches = delivery_id_commit_batches(delivery_ids);
        let Ok(runtime_handle) = tokio::runtime::Handle::try_current() else {
            // 无 runtime 时只能投递无等待命令；tombstone 保留到下次启动重放。
            for delivery_ids in delivery_batches {
                feedback.complete_pending_deliveries(delivery_ids);
            }
            return;
        };
        let stopping = Arc::clone(&self.stopping);
        let cancellation_store = self.cancellation_store.clone();
        let Ok(session_id) = self.current_session_id() else {
            return;
        };
        let delivery_commit_gate = Arc::clone(&self.delivery_commit_gate);
        let _ = self.spawn_registered_task(&runtime_handle, async move {
            for delivery_ids in delivery_batches {
                let _commit_guard = delivery_commit_gate.lock().await;
                let mut retry_delay = Duration::from_millis(100);
                loop {
                    let commit =
                        feedback.commit_pending_deliveries(delivery_ids.clone(), Vec::new());
                    match tokio::time::timeout(Duration::from_secs(2), commit).await {
                        Ok(Ok(())) => {
                            if !cancellation_store
                                .settle_with_retry(&session_id, &delivery_ids, &stopping, &feedback)
                                .await
                            {
                                return;
                            }
                            break;
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "提交 Agent 投递取消失败，稍后重试");
                        }
                        Err(_) => tracing::warn!("提交 Agent 投递取消超时，稍后重试"),
                    }
                    if stopping.load(Ordering::Acquire) || feedback.is_closed() {
                        return;
                    }
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(2));
                }
            }
        });
    }

    /// 重启后把 child session 中已经完成、但父 Session 尚未 ACK 的 outbox 回执重放。
    fn submit_receipt_replays(&self, replays: Vec<crate::team_bridge::PendingReceiptReplay>) {
        if replays.is_empty() || self.stopping.load(Ordering::Acquire) {
            return;
        }
        let Some(feedback) = self.feedback_tx() else {
            return;
        };
        let Ok(runtime_handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let replay_batches = receipt_replay_commit_batches(replays);
        let stopping = Arc::clone(&self.stopping);
        let team = Arc::clone(&self.team);
        let cancellation_store = self.cancellation_store.clone();
        let Ok(parent_session_id) = self.current_session_id() else {
            return;
        };
        let delivery_commit_gate = Arc::clone(&self.delivery_commit_gate);
        let _ = self.spawn_registered_task(&runtime_handle, async move {
            for batch in replay_batches {
                // gate 覆盖完整的 ACK → 永久结算 → outbox 清理，下一批不得越过。
                let _commit_guard = delivery_commit_gate.lock().await;
                let mut retry_delay = Duration::from_millis(100);
                loop {
                    let commit = feedback.commit_pending_deliveries(
                        batch.delivery_ids.clone(),
                        vec![batch.injection.clone()],
                    );
                    match tokio::time::timeout(Duration::from_secs(2), commit).await {
                        Ok(Ok(())) => {
                            if !cancellation_store
                                .settle_with_retry(
                                    &parent_session_id,
                                    &batch.delivery_ids,
                                    &stopping,
                                    &feedback,
                                )
                                .await
                            {
                                return;
                            }
                            crate::team_bridge::remove_acknowledged_main_messages(
                                &team,
                                &batch.main_message_ids,
                            );
                            break;
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "提交恢复的 Agent 回执失败，稍后重试");
                        }
                        Err(_) => tracing::warn!("提交恢复的 Agent 回执超时，稍后重试"),
                    }
                    if stopping.load(Ordering::Acquire) || feedback.is_closed() {
                        return;
                    }
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(2));
                }
            }
        });
    }

    /// 让 lock/unlock 与真实写工具使用同一绝对规范化路径。
    pub(crate) fn normalize_team_tool_call(&self, call: &ToolCall) -> Result<ToolCall, String> {
        if !matches!(call.name.as_str(), "lock_file" | "unlock_file") {
            return Ok(call.clone());
        }
        let raw = call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "文件锁工具缺少 path 参数".to_string())?;
        let workspace = self
            .workspace
            .read()
            .map_err(|_| "工作目录状态锁定失败".to_string())?
            .clone()
            .ok_or_else(|| "当前会话没有可用工作目录".to_string())?;
        let path = tiangong_toolkit::resolve_write_path_from_base(raw, &workspace)
            .map_err(|error| error.to_string())?;
        let mut normalized = call.clone();
        normalized.arguments["path"] = serde_json::Value::String(path.display().to_string());
        Ok(normalized)
    }

    /// 为收件箱非空且尚未调度的 Agent 启动一个串行后台消费者。
    pub(crate) fn schedule_pending_agents(&self, agent_ids: Vec<String>) {
        let Some(scheduler) = self.scheduler_context() else {
            return;
        };
        scheduler.schedule(agent_ids);
    }

    /// 提供可移入 `'static` 工具 Future 的调度句柄。同步 Main Agent 在当前批次完成
    /// ACK、永久结算和 outbox 清理后，用它续跑仍有 inbox 的目标。
    pub(crate) fn scheduler_context(&self) -> Option<AgentSchedulerContext> {
        Some(AgentSchedulerContext {
            team: Arc::clone(&self.team),
            storage_root: self.storage_root_snapshot(),
            runtime_engine: self.runtime_engine_snapshot()?,
            parent_tools: self.parent_tools_snapshot(),
            feedback_tx: self.feedback_tx()?,
            prompt_config: self.prompt_config_snapshot()?,
            execution_semaphore: Arc::clone(&self.execution_semaphore),
            token_budget: Arc::clone(&self.sub_agent_token_budget),
            budget_resume_requested: Arc::clone(&self.budget_resume_requested),
            stopping: Arc::clone(&self.stopping),
            cancellation_store: self.cancellation_store.clone(),
            delivery_commit_gate: Arc::clone(&self.delivery_commit_gate),
            background_tasks: Arc::clone(&self.background_tasks),
        })
    }
}

impl AgentSchedulerContext {
    /// Core 已 ACK 后登记不可由调用方取消的永久结算任务。
    ///
    /// 返回的 receiver 只用于让正常工具路径等待结果；丢弃 receiver 不会取消任务。
    /// 任务始终持有提交门闩，依次完成永久结算与 outbox 清理后才允许下一批提交。
    pub(crate) fn spawn_delivery_finalizer(
        &self,
        commit_guard: tokio::sync::OwnedMutexGuard<()>,
        session_id: String,
        delivery_ids: Vec<String>,
        main_message_ids: Vec<String>,
    ) -> Result<tokio::sync::oneshot::Receiver<bool>, String> {
        let runtime_handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "Agent Team 投递结算缺少 Tokio runtime".to_string())?;
        let cancellation_store = self.cancellation_store.clone();
        let stopping = Arc::clone(&self.stopping);
        let feedback = self.feedback_tx.clone();
        let team = Arc::clone(&self.team);
        spawn_registered_finalizer_inner(
            &self.stopping,
            &self.background_tasks,
            &runtime_handle,
            commit_guard,
            async move {
                let settled = cancellation_store
                    .settle_with_retry(&session_id, &delivery_ids, &stopping, &feedback)
                    .await;
                if settled {
                    crate::team_bridge::remove_acknowledged_main_messages(&team, &main_message_ids);
                }
                settled
            },
        )
    }

    pub(crate) fn schedule(&self, agent_ids: Vec<String>) {
        let agent_ids = crate::team_bridge::pending_agent_ids_for_scheduler(&self.team, &agent_ids);
        if agent_ids.is_empty()
            || self.stopping.load(Ordering::Acquire)
            || self.token_budget.is_paused()
        {
            return;
        }
        let Ok(runtime_handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("Agent Team 后台调度缺少 Tokio runtime");
            return;
        };

        for agent_id in agent_ids {
            let marked = self
                .team
                .lock()
                .map(|mut team| team.try_mark_scheduled(&agent_id))
                .unwrap_or(false);
            if !marked {
                continue;
            }
            let worker = self.clone();
            let scheduled_agent_id = agent_id.clone();
            let spawned = spawn_registered_task_inner(
                &self.stopping,
                &self.background_tasks,
                &runtime_handle,
                worker.run(scheduled_agent_id),
            );
            if !spawned {
                if let Ok(mut team) = self.team.lock() {
                    // 关闭已经越过入口或任务登记失败时，撤销刚才占用的 scheduled 槽；
                    // pending inbox 保持不动，重启后仍可恢复。
                    team.finish_in_flight(&agent_id);
                }
            }
        }
    }

    fn run(
        self,
        scheduled_agent_id: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            if let Ok(mut state) = self.team.lock() {
                state.begin_scheduled(&scheduled_agent_id);
            }
            loop {
                if self.stopping.load(Ordering::Acquire)
                    || self.feedback_tx.is_closed()
                    || self.token_budget.is_paused()
                {
                    break;
                }
                let result = crate::team_bridge::run_agent_turn(
                    Arc::clone(&self.team),
                    scheduled_agent_id.clone(),
                    self.storage_root.clone(),
                    self.runtime_engine.clone(),
                    self.parent_tools.clone(),
                    self.feedback_tx.clone(),
                    Arc::clone(&self.prompt_config),
                    Arc::clone(&self.execution_semaphore),
                    Arc::clone(&self.token_budget),
                )
                .await;
                let made_progress = result.made_progress;
                let completed_delivery_ids = result.completed_delivery_ids;
                let acknowledged_main_message_ids = result
                    .main_messages
                    .iter()
                    .map(|message| message.id.clone())
                    .collect::<Vec<_>>();
                let payload = json!({
                    "agent_id": scheduled_agent_id,
                    "report": result.report,
                    "messages": result.main_messages,
                    "delivery_ids": completed_delivery_ids,
                    "cancelled": result.cancelled,
                });

                if completed_delivery_ids.is_empty() {
                    // 未实际消费消息时没有完成 ID；失败/关闭结果已通过实时通知说明。
                    if made_progress {
                        self.feedback_tx
                            .inject_tool("agent_team_async_report", payload);
                    }
                } else {
                    // 所有成功消费的消息都把报告与稳定完成 ID 原子提交。用户直达
                    // 消息还会删除 pending；普通内部消息则只更新完成账本。
                    let injection = PluginFeedback::new("agent_team_async_report", payload);
                    let _commit_guard = self.delivery_commit_gate.lock().await;
                    let mut retry_delay = Duration::from_millis(100);
                    let committed = loop {
                        let commit = self.feedback_tx.commit_pending_deliveries(
                            completed_delivery_ids.clone(),
                            vec![injection.clone()],
                        );
                        match tokio::time::timeout(Duration::from_secs(2), commit).await {
                            Ok(Ok(())) => break true,
                            Ok(Err(error)) => {
                                tracing::warn!(%error, "提交 Agent 消息结果失败，稍后重试");
                            }
                            Err(_) => {
                                tracing::warn!("提交 Agent 消息结果超时，稍后重试");
                            }
                        }
                        if self.stopping.load(Ordering::Acquire) || self.feedback_tx.is_closed() {
                            break false;
                        }
                        tokio::time::sleep(retry_delay).await;
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(2));
                    };
                    if !committed {
                        break;
                    }
                    if !self
                        .cancellation_store
                        .settle_with_retry(
                            &self.prompt_config.session_id,
                            &completed_delivery_ids,
                            &self.stopping,
                            &self.feedback_tx,
                        )
                        .await
                    {
                        break;
                    }
                    crate::team_bridge::remove_acknowledged_main_messages(
                        &self.team,
                        &acknowledged_main_message_ids,
                    );
                }

                if !made_progress {
                    break;
                }
                let should_continue = self
                    .team
                    .lock()
                    .map(|mut state| state.finish_in_flight_if_idle(&scheduled_agent_id))
                    .unwrap_or(false);
                if !should_continue {
                    self.resume_after_worker_finished();
                    return;
                }
            }

            let (was_paused, pending_agents) = finish_worker_and_take_budget_resume(
                &self.team,
                &scheduled_agent_id,
                &self.token_budget,
                &self.budget_resume_requested,
                &self.stopping,
            );
            self.resume_pending_agents(was_paused, pending_agents);
        })
    }

    /// `finish_in_flight_if_idle` 已在同一临界区清除了当前 worker；这里只消费恢复门控。
    fn resume_after_worker_finished(&self) {
        let (was_paused, pending_agents) = {
            let Ok(team) = self.team.lock() else {
                return;
            };
            if self.stopping.load(Ordering::Acquire)
                || team.has_execution_work()
                || self
                    .budget_resume_requested
                    .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            {
                return;
            }
            let was_paused = self.token_budget.reset();
            (was_paused, team.registry.agent_ids_with_pending_inbox())
        };
        self.resume_pending_agents(was_paused, pending_agents);
    }

    fn resume_pending_agents(&self, was_paused: bool, pending_agents: Vec<String>) {
        if was_paused {
            let event = tiangong_types::StreamEvent::AgentNotification {
                agent_id: "agent-team-budget".to_string(),
                agent_label: "Agent Team".to_string(),
                content: "新的主会话轮次已重置 Sub Agent token 预算，暂停任务正在自动恢复。"
                    .to_string(),
                level: "info".to_string(),
            };
            if !self.feedback_tx.send_turn_stream_event(event.clone()) {
                self.feedback_tx.send_stream_event(event);
            }
        }
        self.schedule(pending_agents);
    }
}

impl Plugin for AgentTeamPlugin {
    fn id(&self) -> &str {
        crate::constants::PLUGIN_ID
    }

    fn register(&self, engine: &RuntimeEngine) {
        // 捕获父 RuntimeEngine clone（子 Agent 经此继承 tool_overrides）。
        if let Ok(mut guard) = self.runtime_engine.write() {
            *guard = Some(engine.clone());
        }
    }

    fn set_workspace(&self, workspace: Option<&Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(Path::to_path_buf);
        }
    }

    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.feedback_tx.write() {
            *guard = Some(tx);
        }
    }

    fn on_session_ready(&self, session: &mut Session) {
        if let Ok(mut guard) = self.session_id.write() {
            *guard = Some(session.id.clone());
        }
        // 从 engine 的 tool_spec_providers 收集全部工具规格，供子 Agent 过滤可用工具。
        self.refresh_parent_tools();
        // 构建 PromptConfig（需要 models_config + agent_config）。
        self.rebuild_prompt_config(session);
        // 协议账本读取失败时必须 fail-closed：父 pending 与 child receipt 均保持原样，
        // 但本次不恢复、不调度，避免把已经取消的工作重新执行。
        let protocol_state = match self.cancellation_store.load_state(&session.id) {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(%error, "恢复 Agent 投递协议账本失败，暂停团队投递");
                self.notify_cancellation_persist_failure(&error);
                return;
            }
        };
        // 父完成账本仍有记录时先永久结算。成功后即使 Core 的有界完成记录淘汰，
        // 对应 child receipt 也不会再次进入恢复队列。
        let protocol_state = if session.completed_plugin_delivery_ids.is_empty() {
            protocol_state
        } else {
            match self
                .cancellation_store
                .settle(&session.id, &session.completed_plugin_delivery_ids)
            {
                Ok(state) => state,
                Err(error) => {
                    tracing::warn!(%error, "恢复 Agent 投递永久结算失败，暂停团队投递");
                    self.notify_cancellation_persist_failure(&error);
                    return;
                }
            }
        };
        let tombstoned_delivery_ids = protocol_state.cancelled_ids;
        let settled_delivery_ids = protocol_state.settled_ids;
        let pending_internal_deliveries = protocol_state.pending_internal_deliveries;
        self.submit_recorded_delivery_cancellation(
            tombstoned_delivery_ids.iter().cloned().collect(),
        );

        // 如果上次崩溃发生在“解散记录落盘”之后、“投递取消确认落盘”之前，
        // 用稳定 terminated ID 重放取消提交，避免父 Session 留下永久悬空投递。
        let terminated = explicitly_terminated_agent_ids(session);
        let mut cancelled_delivery_ids = session
            .pending_plugin_deliveries
            .iter()
            .filter(|delivery| {
                (delivery.plugin_id.is_empty() || delivery.plugin_id == crate::constants::PLUGIN_ID)
                    && terminated.contains(&delivery.target_id)
            })
            .map(|delivery| delivery.delivery_id.clone())
            .collect::<Vec<_>>();
        // 从会话历史恢复 Agent（崩溃恢复）。
        let tools = self.parent_tools_snapshot();
        if let Ok(mut team) = self.team.lock() {
            restore_agents_from_session_history(&mut team, session, &tools, self.storage_root());
        }

        // 内部消息先依赖上一步恢复目标 Agent，再决定恢复或取消。目标已终止/缺失时
        // 必须先把稳定 ID 原子移入取消集合，不能留下永远无法消费的 durable 工作。
        let (internal_deliveries_to_restore, terminated_internal_ids) = match self.team.lock() {
            Ok(team) => pending_internal_deliveries
                .into_iter()
                .partition::<Vec<_>, _>(|(_, delivery)| {
                    !terminated.contains(&delivery.target_agent_id)
                        && team
                            .registry
                            .get(&delivery.target_agent_id)
                            .is_some_and(|agent| agent.status != crate::AgentStatus::Terminated)
                }),
            Err(_) => {
                tracing::warn!("恢复 Agent 内部投递时团队状态锁定失败，暂停团队投递");
                return;
            }
        };
        cancelled_delivery_ids.extend(
            terminated_internal_ids
                .into_iter()
                .map(|(delivery_id, _)| delivery_id),
        );
        cancelled_delivery_ids.sort();
        cancelled_delivery_ids.dedup();
        if let Err(error) = self.persist_delivery_cancellation(&cancelled_delivery_ids) {
            tracing::warn!(%error, "持久化恢复期 Agent 投递取消失败，暂停团队投递");
            self.notify_cancellation_persist_failure(&error);
            return;
        }
        self.submit_recorded_delivery_cancellation(cancelled_delivery_ids.clone());
        let mut cancelled_for_restore = tombstoned_delivery_ids;
        cancelled_for_restore.extend(cancelled_delivery_ids);
        let cancelled_for_scan = cancelled_for_restore.iter().cloned().collect::<Vec<_>>();
        let settled_for_scan = settled_delivery_ids.iter().cloned().collect::<Vec<_>>();
        let mut excluded_for_restore = cancelled_for_restore.clone();
        excluded_for_restore.extend(settled_delivery_ids.iter().cloned());
        let (pending_agents, receipt_replays) = if let Ok(mut team) = self.team.lock() {
            restore_pending_agent_deliveries(&mut team, session);
            for (delivery_id, delivery) in internal_deliveries_to_restore {
                let mut entry = delivery.entry;
                entry.message.id = delivery_id;
                entry.message.to = delivery.target_agent_id.clone();
                team.registry
                    .deliver_inbox_entry(&delivery.target_agent_id, entry);
            }
            team.registry.remove_delivery_ids(&excluded_for_restore);
            let receipt_replays = crate::team_bridge::pending_receipt_replays(
                &team,
                &session.completed_plugin_delivery_ids,
                &cancelled_for_scan,
                &settled_for_scan,
            );
            let replay_ids = receipt_replays
                .iter()
                .map(|replay| replay.delivery_id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            team.registry.remove_delivery_ids(&replay_ids);
            (
                team.registry.agent_ids_with_pending_inbox(),
                receipt_replays,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        self.submit_receipt_replays(receipt_replays);
        self.schedule_pending_agents(pending_agents);
    }

    fn on_engine_rebuilt(&self, session: &mut Session) {
        // engine 重建后工具列表可能变化（插件增减），重新收集。
        self.refresh_parent_tools();
        self.rebuild_prompt_config(session);
    }

    fn on_turn_started(&self, _session: &mut Session, _turn_start_idx: usize) {
        let (was_paused, pending_agents) = self.reset_budget_for_main_turn_if_idle();
        if was_paused {
            self.notify_budget_resumed();
        }
        self.schedule_pending_agents(pending_agents);
    }

    fn plan_plugin_deliveries(
        &self,
        actor_id: &str,
        source_message_id: &str,
        prepared: &[ContentBlock],
    ) -> Vec<PendingPluginDelivery> {
        if actor_id != MAIN_AGENT_ID {
            return Vec::new();
        }
        self.team
            .lock()
            .map(|team| plan_user_mention_deliveries(&team, source_message_id, prepared))
            .unwrap_or_default()
    }

    fn dispatch_plugin_deliveries(&self, session: &Session, source_message_id: &str) -> bool {
        if self.stopping.load(Ordering::Acquire) {
            return false;
        }
        // 直达消息在 on_turn_started 之前派发；先按“新主消息 + 团队空闲”恢复预算，
        // 避免暂停队列在本轮被 schedule gate 静默跳过。
        let (was_paused, _) = self.reset_budget_for_main_turn_if_idle();
        if was_paused {
            self.notify_budget_resumed();
        }
        let (stream_tx, stream_rx) = std::sync::mpsc::channel();
        let (dispatched, targets) = match self.team.lock() {
            Ok(mut team) => {
                let dispatched = dispatch_pending_agent_deliveries(
                    &mut team,
                    session,
                    source_message_id,
                    &stream_tx,
                );
                (dispatched, team.registry.agent_ids_with_pending_inbox())
            }
            Err(_) => (false, Vec::new()),
        };
        drop(stream_tx);
        if let Some(feedback) = self.feedback_tx() {
            for event in stream_rx {
                if !feedback.send_turn_stream_event(event.clone()) {
                    feedback.send_stream_event(event);
                }
            }
        }
        if dispatched {
            self.schedule_pending_agents(targets);
        }
        dispatched
    }

    fn guard_tool_call(
        &self,
        actor_id: &str,
        call: &ToolCall,
        write_targets: &[PathBuf],
    ) -> Result<(), String> {
        // Main 的只读及纯计算工具没有团队策略可检查，直接返回。Sub Agent 即使
        // 不写文件也仍须经过存活状态与工具 allowlist 校验，不能把模型 schema
        // 当作授权边界。
        if actor_id == MAIN_AGENT_ID && write_targets.is_empty() {
            return Ok(());
        }
        let mut team = self
            .team
            .lock()
            .map_err(|_| "团队状态锁定失败".to_string())?;

        if actor_id != MAIN_AGENT_ID {
            let descriptor = team
                .registry
                .get(actor_id)
                .ok_or_else(|| format!("未知子 Agent：{actor_id}"))?;
            if descriptor.status == crate::AgentStatus::Terminated {
                return Err(format!("子 Agent 已终止：{actor_id}"));
            }
            if !descriptor.tools.iter().any(|tool| tool == &call.name) {
                return Err(format!("子 Agent 未获授权使用工具：{}", call.name));
            }
        }

        // 只读调用完成授权检查后即可返回，避免为释放过期锁竞争后续路径。
        // 过期锁会在下一次写入或显式锁操作时清理。
        if write_targets.is_empty() {
            return Ok(());
        }

        let now = chrono::Local::now().naive_local();
        let expired_locks = team.file_locks.release_expired(&now);
        if let Some(feedback) = self.feedback_tx() {
            for (path, expired) in expired_locks {
                let holder_agent_label = team
                    .registry
                    .get(&expired.holder)
                    .map(|descriptor| descriptor.label.clone());
                let event = tiangong_types::StreamEvent::FileLockChanged {
                    path: path.display().to_string(),
                    holder_agent_id: Some(expired.holder),
                    holder_agent_label,
                    action: "expired".to_string(),
                };
                if !feedback.send_turn_stream_event(event.clone()) {
                    feedback.send_stream_event(event);
                }
            }
        }
        if actor_id == MAIN_AGENT_ID {
            return Ok(());
        }
        if call.name == "spawn_task" {
            return Err(
                "Sub Agent 不得启动脱离当前轮次的后台任务；请改用前台命令，确保文件锁覆盖完整执行周期"
                    .to_string(),
            );
        }
        if matches!(
            call.name.as_str(),
            "run_command" | "run_shell" | "terminal_send"
        ) {
            let workspace = self
                .workspace
                .read()
                .map_err(|_| "工作目录状态锁定失败".to_string())?
                .clone()
                .ok_or_else(|| "当前会话没有可用工作目录".to_string())?;
            crate::command_safety::guard_sub_agent_command(call, &workspace)?;
        }
        for path in write_targets {
            team.file_locks.ensure_can_write(path, actor_id, &now)?;
        }
        Ok(())
    }

    fn handle_runtime_command(&self, command: &Command) -> bool {
        match command {
            Command::Approval {
                request_id,
                approved,
            } => {
                let handles = self
                    .team
                    .lock()
                    .map(|team| team.active_agent_handles())
                    .unwrap_or_default();
                for handle in &handles {
                    let _ = handle.command_tx.send(Command::Approval {
                        request_id: request_id.clone(),
                        approved: *approved,
                    });
                }
                !handles.is_empty()
            }
            Command::PluginControl {
                plugin_id,
                action,
                payload,
            } if plugin_id == crate::constants::PLUGIN_ID
                && action == crate::constants::CONTROL_CANCEL_AGENT =>
            {
                let Some(role) = payload.get("role").and_then(serde_json::Value::as_str) else {
                    return false;
                };
                let delivery_ids = match self.team.lock() {
                    Ok(team) => {
                        let Some(agent) = team.registry.find_by_role(role) else {
                            return false;
                        };
                        let Some(handle) = team.active_agent_handle(&agent.agent_id) else {
                            return false;
                        };
                        let delivery_ids = handle
                            .pending_delivery_id
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>();
                        if let Err(error) = self.persist_delivery_cancellation(&delivery_ids) {
                            drop(team);
                            tracing::warn!(%error, "持久化定向 Agent 取消失败，任务保持运行");
                            self.notify_cancellation_persist_failure(&error);
                            return false;
                        }
                        handle.cancel_flag.store(true, Ordering::Release);
                        let _ = handle.command_tx.send(Command::Cancel);
                        delivery_ids
                    }
                    Err(_) => return false,
                };
                self.submit_recorded_delivery_cancellation(delivery_ids);
                true
            }
            Command::PluginControl { .. } => false,
            Command::Cancel => {
                let (delivery_ids, had_work) = {
                    let Ok(mut team) = self.team.lock() else {
                        return false;
                    };
                    let handles = team.active_agent_handles();
                    let had_work = !handles.is_empty() || team.registry.has_pending_inbox();
                    let mut delivery_ids = team.registry.all_pending_delivery_ids();
                    for handle in &handles {
                        if let Some(delivery_id) = &handle.pending_delivery_id {
                            delivery_ids.push(delivery_id.clone());
                        }
                    }
                    delivery_ids.sort();
                    delivery_ids.dedup();
                    if let Err(error) = self.persist_delivery_cancellation(&delivery_ids) {
                        drop(team);
                        tracing::warn!(%error, "持久化全局 Agent 取消失败，团队任务保持运行");
                        self.notify_cancellation_persist_failure(&error);
                        return false;
                    }
                    team.registry.cancel_all_pending_deliveries();
                    for handle in &handles {
                        handle.cancel_flag.store(true, Ordering::Release);
                        let _ = handle.command_tx.send(Command::Cancel);
                    }
                    (delivery_ids, had_work)
                };
                self.submit_recorded_delivery_cancellation(delivery_ids.clone());
                had_work || !delivery_ids.is_empty()
            }
            Command::Shutdown => self
                .team
                .lock()
                .map(|team| {
                    let handles = team.active_agent_handles();
                    for handle in &handles {
                        handle.cancel_flag.store(true, Ordering::Release);
                        handle.shutdown_flag.store(true, Ordering::Release);
                        let _ = handle.command_tx.send(Command::Shutdown);
                    }
                    !handles.is_empty()
                })
                .unwrap_or(false),
            _ => false,
        }
    }

    fn shutdown<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.stopping.store(true, Ordering::Release);
            let _ = self.handle_runtime_command(&Command::Shutdown);
            let tasks = self
                .background_tasks
                .lock()
                .map(|mut tasks| std::mem::take(&mut *tasks))
                .unwrap_or_default();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            for mut task in tasks {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() || tokio::time::timeout(remaining, &mut task).await.is_err()
                {
                    task.abort();
                    let _ = task.await;
                }
            }
            if let Ok(mut team) = self.team.lock() {
                let mut agent_ids = team.registry.agent_ids_with_pending_inbox();
                agent_ids.extend(team.active_agent_ids());
                agent_ids.sort();
                agent_ids.dedup();
                for agent_id in agent_ids {
                    team.finish_in_flight(&agent_id);
                    team.unregister_active_agent(&agent_id);
                    team.registry
                        .update_status(&agent_id, crate::AgentStatus::Idle);
                }
            }
        })
    }

    fn tool_permission_overrides(
        &self,
    ) -> std::collections::BTreeMap<String, tiangong_core::permission::PermissionLevel> {
        // 团队工具均为无副作用的管理操作（创建/解散 Agent、消息路由、通知、文件锁），
        // 声明为 Safe 避免 core 默认 classify_tool 把未知工具名归为 Critical（需要审批）。
        let mut overrides = std::collections::BTreeMap::new();
        for name in [
            "create_agent",
            "dismiss_agent",
            "send_message",
            "broadcast_message",
            "notify_user",
            "lock_file",
            "unlock_file",
        ] {
            overrides.insert(
                name.to_string(),
                tiangong_core::permission::PermissionLevel::Safe,
            );
        }
        overrides
    }
}

impl AgentTeamPlugin {
    /// 构建/刷新 PromptConfig（从父 RuntimeEngine 的配置 + 当前 session）。
    fn rebuild_prompt_config(&self, session: &Session) {
        let Some(engine) = self.runtime_engine.read().ok().and_then(|g| g.clone()) else {
            return;
        };
        let mut base =
            SystemPromptConfig::from_configs(engine.models_config(), engine.agent_config());
        base.plugin_sections = engine.collect_plugin_prompt_sections();
        let prompt_config = Arc::new(PromptConfig {
            session_id: session.id.clone(),
            base: Arc::new(base),
        });
        if let Ok(mut guard) = self.prompt_config.write() {
            *guard = Some(prompt_config);
        }
    }

    /// 从父 RuntimeEngine 的 tool_spec_providers 收集全部工具规格。
    ///
    /// 子 Agent 创建时可选指定 tools 列表（默认继承全部），run_agent_turn 用此快照
    /// 过滤出子 Agent 可用工具。engine 重建后（插件增减导致工具变化）需重新收集。
    fn refresh_parent_tools(&self) {
        let Some(engine) = self.runtime_engine.read().ok().and_then(|g| g.clone()) else {
            return;
        };
        let mut seen = std::collections::HashSet::new();
        let tools: Vec<ToolSpec> = engine
            .tool_spec_providers()
            .iter()
            .flat_map(|provider| provider.tool_specs())
            .filter(|tool| seen.insert(tool.name.clone()))
            .collect();
        if let Ok(mut guard) = self.parent_tools.write() {
            *guard = tools;
        }
    }
}

/// 注入团队工具使用指引（迁自 core `prompt/sections.rs::build_agent_team_section`）。
impl PromptSectionProvider for AgentTeamPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        vec![build_agent_team_section()]
    }
}

fn build_agent_team_section() -> String {
    "团队协作能力（可选使用）：
当任务复杂需要分工时，你可以创建团队来协作完成。以下是可用的团队工具：

- create_agent(role, label, system_prompt, tools)：创建一个 Sub Agent
  - role：角色标识（如 pm、dev、test），用于消息路由
  - label：显示名称（如「项目经理」「开发者」）
  - system_prompt：Agent 的专属指令
  - tools：可选，指定可用工具列表（默认继承你的工具集，不含 create_agent/dismiss_agent）
  - Agent 持续存在直到被解散
  - 最多同时 8 个 Agent

- send_message(to, content)：向指定角色的 Agent 发送消息，Agent 会自动执行任务
- broadcast_message(content, exclude)：向所有 Agent 广播消息
- notify_user(content, level)：向用户推送通知（info/warning/error）

- lock_file(path)：获取文件编辑锁（编辑前必先加锁，防止多 Agent 冲突）
- unlock_file(path)：释放文件编辑锁
- dismiss_agent(role)：解散指定 Agent，释放其持有的所有资源

使用要点：
1. 复杂任务先拆解，为每个子任务创建专职 Agent（明确的 system_prompt）。
2. 通过 send_message 分配任务；send_message 会等待目标 Agent 执行完成并把其汇报作为结果返回给你。
3. 用户输入中的 @提及由系统直接投递；不要再次调用 send_message 造成重复任务。
4. Sub Agent 修改任何文件前都必须 lock_file，修改后 unlock_file；补丁涉及多个文件时必须逐个加锁。
5. Sub Agent 调用 run_command 或 run_shell 前必须先 lock_file(\".\") 获取工作区锁，显式设置 1..=240 秒 timeout，命令结束后再释放；不得使用 terminal_send、spawn_task、后台命令或交互式命令。
"
    .to_string()
}

// 静默未使用 import 警告（MediaAsset/StreamEvent 在其他模块使用）。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::SUB_AGENT_TOTAL_TOKEN_BUDGET;
    use crate::state::message_bus::AgentInboxEntry;
    use crate::{AgentDescriptor, AgentMessage, AgentStatus, MessagePriority};

    fn make_tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: "tool-call".to_string(),
            name: name.to_string(),
            arguments: serde_json::json!({}),
        }
    }

    fn append_agent_history(session: &mut Session, agent_id: &str) {
        let mut message = tiangong_core::session::Message::new(
            tiangong_core::session::MessageRole::System,
            format!("[Agent] Developer (dev) 已加入团队 id={agent_id}"),
        );
        message.model_excluded = true;
        session.messages.push(message);
    }

    fn internal_entry(delivery_id: &str, agent_id: &str) -> AgentInboxEntry {
        AgentInboxEntry {
            message: AgentMessage {
                id: delivery_id.to_string(),
                from: "main".to_string(),
                to: agent_id.to_string(),
                content: "durable internal work".to_string(),
                priority: MessagePriority::Normal,
                created_at: "2026-07-12 12:00:00".to_string(),
            },
            additional_content: Vec::new(),
            session_message_id: None,
        }
    }

    fn receipt_replay(index: usize) -> crate::team_bridge::PendingReceiptReplay {
        crate::team_bridge::PendingReceiptReplay {
            agent_id: format!("agent-{}", index % crate::constants::MAX_AGENTS),
            delivery_id: format!("delivery-{index}"),
            report: format!("report-{index}"),
            main_messages: vec![AgentMessage {
                id: format!("main-message-{index}"),
                from: "agent-dev".to_string(),
                to: "main".to_string(),
                content: format!("message-{index}"),
                priority: MessagePriority::Normal,
                created_at: "2026-07-12 12:00:00".to_string(),
            }],
        }
    }

    #[test]
    fn delivery_id_backlog_is_split_at_core_commit_boundary() {
        let batches = delivery_id_commit_batches(
            (0..=MAX_PLUGIN_DELIVERIES_PER_COMMIT)
                .map(|index| format!("delivery-{index}"))
                .collect(),
        );

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), MAX_PLUGIN_DELIVERIES_PER_COMMIT);
        assert_eq!(
            batches[1],
            [format!("delivery-{MAX_PLUGIN_DELIVERIES_PER_COMMIT}")]
        );
    }

    #[test]
    fn receipt_replay_batches_build_independent_payloads() {
        let batches = receipt_replay_commit_batches(
            (0..=MAX_PLUGIN_DELIVERIES_PER_COMMIT)
                .map(receipt_replay)
                .collect(),
        );

        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches[0].delivery_ids.len(),
            MAX_PLUGIN_DELIVERIES_PER_COMMIT
        );
        assert_eq!(
            batches[0].main_message_ids.len(),
            MAX_PLUGIN_DELIVERIES_PER_COMMIT
        );
        assert_eq!(
            batches[1].delivery_ids,
            [format!("delivery-{MAX_PLUGIN_DELIVERIES_PER_COMMIT}")]
        );
        assert_eq!(
            batches[1].main_message_ids,
            [format!("main-message-{MAX_PLUGIN_DELIVERIES_PER_COMMIT}")]
        );

        let first_ids = batches[0].injection.payload["delivery_ids"]
            .as_array()
            .unwrap();
        let first_reports = batches[0].injection.payload["reports"].as_array().unwrap();
        let second_ids = batches[1].injection.payload["delivery_ids"]
            .as_array()
            .unwrap();
        let second_reports = batches[1].injection.payload["reports"].as_array().unwrap();
        assert_eq!(first_ids.len(), MAX_PLUGIN_DELIVERIES_PER_COMMIT);
        assert_eq!(first_reports.len(), MAX_PLUGIN_DELIVERIES_PER_COMMIT);
        assert_eq!(second_ids.len(), 1);
        assert_eq!(second_reports.len(), 1);
        assert_eq!(
            second_ids[0],
            serde_json::Value::String(format!("delivery-{MAX_PLUGIN_DELIVERIES_PER_COMMIT}"))
        );
        assert_eq!(
            second_reports[0]["delivery_id"],
            serde_json::Value::String(format!("delivery-{MAX_PLUGIN_DELIVERIES_PER_COMMIT}"))
        );
    }

    #[test]
    fn next_main_turn_resets_paused_budget_and_exposes_pending_agents() {
        let storage = tempfile::TempDir::new().unwrap();
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let agent_id = "agent-dev";
        let mut team = plugin.team.lock().unwrap();
        team.registry.register(AgentDescriptor {
            agent_id: agent_id.to_string(),
            role: "dev".to_string(),
            label: "Developer".to_string(),
            system_prompt: "work".to_string(),
            tools: Vec::new(),
            status: AgentStatus::Idle,
        });
        team.registry.deliver_message(
            agent_id,
            AgentMessage {
                id: "message-1".to_string(),
                from: "main".to_string(),
                to: agent_id.to_string(),
                content: "continue".to_string(),
                priority: MessagePriority::Normal,
                created_at: "2026-07-12 12:00:00".to_string(),
            },
        );
        drop(team);
        plugin
            .sub_agent_token_budget
            .record_usage(SUB_AGENT_TOTAL_TOKEN_BUDGET);
        assert!(plugin.sub_agent_token_budget.is_paused());

        let (was_paused, pending_agents) = plugin.reset_budget_for_main_turn_if_idle();

        assert!(was_paused);
        assert_eq!(pending_agents, [agent_id]);
        assert_eq!(plugin.sub_agent_token_budget.used_tokens(), 0);
        assert!(!plugin.sub_agent_token_budget.is_paused());
    }

    #[test]
    fn main_turn_resume_request_survives_until_last_worker_settles() {
        let storage = tempfile::TempDir::new().unwrap();
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let agent_id = "agent-dev";
        let mut team = plugin.team.lock().unwrap();
        team.registry.register(AgentDescriptor {
            agent_id: agent_id.to_string(),
            role: "dev".to_string(),
            label: "Developer".to_string(),
            system_prompt: "work".to_string(),
            tools: Vec::new(),
            status: AgentStatus::Idle,
        });
        team.registry.deliver_message(
            agent_id,
            AgentMessage {
                id: "message-resume".to_string(),
                from: "main".to_string(),
                to: agent_id.to_string(),
                content: "continue".to_string(),
                priority: MessagePriority::Normal,
                created_at: "2026-07-12 12:00:00".to_string(),
            },
        );
        assert!(team.try_mark_scheduled(agent_id));
        team.begin_scheduled(agent_id);
        drop(team);
        plugin
            .sub_agent_token_budget
            .record_usage(SUB_AGENT_TOTAL_TOKEN_BUDGET);

        // 新主 turn 已到达，但旧 worker 尚未清除 in-flight：此时只记录恢复请求。
        assert_eq!(
            plugin.reset_budget_for_main_turn_if_idle(),
            (false, Vec::new())
        );
        assert!(plugin.budget_resume_requested.load(Ordering::Acquire));
        assert!(plugin.sub_agent_token_budget.is_paused());

        // 最后一个 worker 收敛后消费一次请求，重置预算并交回所有 pending 目标。
        let (was_paused, pending_agents) = finish_worker_and_take_budget_resume(
            &plugin.team,
            agent_id,
            &plugin.sub_agent_token_budget,
            &plugin.budget_resume_requested,
            &plugin.stopping,
        );
        assert!(was_paused);
        assert_eq!(pending_agents, [agent_id]);
        assert!(!plugin.budget_resume_requested.load(Ordering::Acquire));
        assert!(!plugin.sub_agent_token_budget.is_paused());
        assert!(!plugin.team.lock().unwrap().has_execution_work());
    }

    #[test]
    fn shutdown_does_not_consume_resume_request_or_restart_pending_work() {
        let storage = tempfile::TempDir::new().unwrap();
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let agent_id = "agent-dev";
        let mut team = plugin.team.lock().unwrap();
        team.registry.register(AgentDescriptor {
            agent_id: agent_id.to_string(),
            role: "dev".to_string(),
            label: "Developer".to_string(),
            system_prompt: "work".to_string(),
            tools: Vec::new(),
            status: AgentStatus::Idle,
        });
        team.registry.deliver_message(
            agent_id,
            AgentMessage {
                id: "message-shutdown".to_string(),
                from: "main".to_string(),
                to: agent_id.to_string(),
                content: "continue".to_string(),
                priority: MessagePriority::Normal,
                created_at: "2026-07-12 12:00:00".to_string(),
            },
        );
        assert!(team.try_mark_scheduled(agent_id));
        team.begin_scheduled(agent_id);
        drop(team);
        plugin
            .sub_agent_token_budget
            .record_usage(SUB_AGENT_TOTAL_TOKEN_BUDGET);
        plugin
            .budget_resume_requested
            .store(true, Ordering::Release);
        plugin.stopping.store(true, Ordering::Release);

        let (was_paused, pending_agents) = finish_worker_and_take_budget_resume(
            &plugin.team,
            agent_id,
            &plugin.sub_agent_token_budget,
            &plugin.budget_resume_requested,
            &plugin.stopping,
        );
        assert!(!was_paused);
        assert!(pending_agents.is_empty());
        assert!(plugin.budget_resume_requested.load(Ordering::Acquire));
        assert!(plugin.sub_agent_token_budget.is_paused());
        assert!(!plugin.team.lock().unwrap().has_execution_work());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_for_registered_tasks_and_rejects_late_spawns() {
        let storage = tempfile::TempDir::new().unwrap();
        let plugin = Arc::new(AgentTeamPlugin::new(storage.path().to_path_buf()));
        let completed = Arc::new(AtomicBool::new(false));
        let completed_in_task = Arc::clone(&completed);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();

        assert!(
            plugin.spawn_registered_task(&tokio::runtime::Handle::current(), async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                completed_in_task.store(true, Ordering::Release);
            })
        );
        started_rx.await.unwrap();

        let shutdown_plugin = Arc::clone(&plugin);
        let mut shutdown_task = tokio::spawn(async move {
            <AgentTeamPlugin as Plugin>::shutdown(shutdown_plugin.as_ref()).await;
        });
        tokio::task::yield_now().await;
        assert!(!shutdown_task.is_finished());

        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), &mut shutdown_task)
            .await
            .expect("shutdown 应等待已登记任务结束")
            .unwrap();
        assert!(completed.load(Ordering::Acquire));

        let late_task_ran = Arc::new(AtomicBool::new(false));
        let late_task_flag = Arc::clone(&late_task_ran);
        assert!(
            !plugin.spawn_registered_task(&tokio::runtime::Handle::current(), async move {
                late_task_flag.store(true, Ordering::Release);
            },)
        );
        tokio::task::yield_now().await;
        assert!(!late_task_ran.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_finalizer_receiver_still_settles_and_cleans_outbox() {
        let storage = tempfile::TempDir::new().unwrap();
        let cancellation_store = CancellationTombstoneStore::new(storage.path().to_path_buf());
        let session_id = "session-finalizer";
        let delivery_id = "delivery-finalizer".to_string();
        cancellation_store
            .record_cancelled(session_id, [delivery_id.clone()])
            .unwrap();

        let team = Arc::new(Mutex::new(TeamContext::new()));
        let main_message_id = "main-message-finalizer".to_string();
        team.lock().unwrap().main_inbox.push(AgentMessage {
            id: main_message_id.clone(),
            from: "agent-dev".to_string(),
            to: "main".to_string(),
            content: "done".to_string(),
            priority: MessagePriority::Normal,
            created_at: "2026-07-12 12:00:00".to_string(),
        });

        let stopping = AtomicBool::new(false);
        let background_tasks = Mutex::new(Vec::new());
        let commit_gate = Arc::new(tokio::sync::Mutex::new(()));
        let commit_guard = Arc::clone(&commit_gate).lock_owned().await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let store_for_task = cancellation_store.clone();
        let team_for_task = Arc::clone(&team);
        let id_for_task = delivery_id.clone();
        let message_for_task = main_message_id.clone();
        let receiver = spawn_registered_finalizer_inner(
            &stopping,
            &background_tasks,
            &tokio::runtime::Handle::current(),
            commit_guard,
            async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                store_for_task
                    .settle(session_id, std::slice::from_ref(&id_for_task))
                    .unwrap();
                crate::team_bridge::remove_acknowledged_main_messages(
                    &team_for_task,
                    &[message_for_task],
                );
                true
            },
        )
        .unwrap();

        started_rx.await.unwrap();
        drop(receiver);
        assert!(commit_gate.try_lock().is_err());
        release_tx.send(()).unwrap();
        let task = background_tasks.lock().unwrap().pop().unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("丢弃等待者后结算任务仍应完成")
            .unwrap();

        let protocol = cancellation_store.load_state(session_id).unwrap();
        assert!(protocol.cancelled_ids.is_empty());
        assert!(protocol.settled_ids.contains(&delivery_id));
        assert!(team.lock().unwrap().main_inbox.is_empty());
        assert!(commit_gate.try_lock().is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_rejects_late_finalizer_and_releases_commit_gate() {
        let stopping = AtomicBool::new(true);
        let background_tasks = Mutex::new(Vec::new());
        let commit_gate = Arc::new(tokio::sync::Mutex::new(()));
        let commit_guard = Arc::clone(&commit_gate).lock_owned().await;
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_task = Arc::clone(&ran);

        let result = spawn_registered_finalizer_inner(
            &stopping,
            &background_tasks,
            &tokio::runtime::Handle::current(),
            commit_guard,
            async move {
                ran_in_task.store(true, Ordering::Release);
                true
            },
        );

        let error = match result {
            Ok(_) => panic!("关闭后不应登记新的投递结算任务"),
            Err(error) => error,
        };
        assert!(error.contains("正在关闭"));
        assert!(!ran.load(Ordering::Acquire));
        assert!(background_tasks.lock().unwrap().is_empty());
        assert!(commit_gate.try_lock().is_ok());
    }

    #[test]
    fn sub_agent_must_lock_every_declared_write_target() {
        let storage = tempfile::TempDir::new().unwrap();
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let agent_id = "agent-dev";
        plugin
            .team
            .lock()
            .unwrap()
            .registry
            .register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "edit files".to_string(),
                tools: vec!["apply_patch".to_string()],
                status: AgentStatus::Idle,
            });

        let first = storage.path().join("first.txt");
        let second = storage.path().join("second.txt");
        let call = make_tool_call("apply_patch");
        let targets = vec![first.clone(), second.clone()];

        let error =
            <AgentTeamPlugin as Plugin>::guard_tool_call(&plugin, agent_id, &call, &targets)
                .unwrap_err();
        assert!(error.contains("尚未加锁"));

        let now = chrono::Local::now().naive_local();
        plugin
            .team
            .lock()
            .unwrap()
            .file_locks
            .try_lock(&first, agent_id, &now)
            .unwrap();
        let error =
            <AgentTeamPlugin as Plugin>::guard_tool_call(&plugin, agent_id, &call, &targets)
                .unwrap_err();
        assert!(error.contains("second.txt"));

        plugin
            .team
            .lock()
            .unwrap()
            .file_locks
            .try_lock(&second, agent_id, &now)
            .unwrap();
        assert!(
            <AgentTeamPlugin as Plugin>::guard_tool_call(&plugin, agent_id, &call, &targets,)
                .is_ok()
        );
    }

    #[test]
    fn write_free_tool_call_does_not_require_a_lock() {
        let storage = tempfile::TempDir::new().unwrap();
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let agent_id = "agent-dev";
        plugin
            .team
            .lock()
            .unwrap()
            .registry
            .register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "inspect files".to_string(),
                tools: vec!["apply_patch".to_string()],
                status: AgentStatus::Idle,
            });

        assert!(<AgentTeamPlugin as Plugin>::guard_tool_call(
            &plugin,
            agent_id,
            &make_tool_call("apply_patch"),
            &[],
        )
        .is_ok());
    }

    #[test]
    fn write_free_tool_still_requires_sub_agent_allowlist() {
        let storage = tempfile::TempDir::new().unwrap();
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let agent_id = "agent-dev";
        plugin
            .team
            .lock()
            .unwrap()
            .registry
            .register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "inspect files".to_string(),
                tools: vec!["read_file".to_string()],
                status: AgentStatus::Idle,
            });

        let error = <AgentTeamPlugin as Plugin>::guard_tool_call(
            &plugin,
            agent_id,
            &make_tool_call("fetch"),
            &[],
        )
        .unwrap_err();
        assert!(error.contains("未获授权"));
    }

    #[test]
    fn command_requires_workspace_lock_and_background_task_is_rejected() {
        let storage = tempfile::TempDir::new().unwrap();
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let agent_id = "agent-dev";
        plugin
            .team
            .lock()
            .unwrap()
            .registry
            .register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "run checks".to_string(),
                tools: vec!["run_command".to_string(), "spawn_task".to_string()],
                status: AgentStatus::Idle,
            });
        let workspace = storage.path().to_path_buf();
        <AgentTeamPlugin as Plugin>::set_workspace(&plugin, Some(&workspace));
        let mut command_call = make_tool_call("run_command");
        command_call.arguments = serde_json::json!({
            "cmd": "cargo",
            "args": ["check"],
            "timeout": 60,
        });

        assert!(<AgentTeamPlugin as Plugin>::guard_tool_call(
            &plugin,
            agent_id,
            &command_call,
            std::slice::from_ref(&workspace),
        )
        .is_err());
        plugin
            .team
            .lock()
            .unwrap()
            .file_locks
            .try_lock(&workspace, agent_id, &chrono::Local::now().naive_local())
            .unwrap();
        assert!(<AgentTeamPlugin as Plugin>::guard_tool_call(
            &plugin,
            agent_id,
            &command_call,
            std::slice::from_ref(&workspace),
        )
        .is_ok());
        let error = <AgentTeamPlugin as Plugin>::guard_tool_call(
            &plugin,
            agent_id,
            &make_tool_call("spawn_task"),
            std::slice::from_ref(&workspace),
        )
        .unwrap_err();
        assert!(error.contains("后台任务"));
    }

    #[test]
    fn restart_does_not_restore_a_tombstoned_delivery() {
        let storage = tempfile::TempDir::new().unwrap();
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let agent_id = "agent-dev";
        plugin
            .team
            .lock()
            .unwrap()
            .registry
            .register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "work".to_string(),
                tools: Vec::new(),
                status: AgentStatus::Idle,
            });
        let mut parent = Session::new("parent");
        parent
            .pending_plugin_deliveries
            .push(PendingPluginDelivery {
                delivery_id: "delivery-cancelled".to_string(),
                source_message_id: "message-1".to_string(),
                plugin_id: crate::constants::PLUGIN_ID.to_string(),
                target_id: agent_id.to_string(),
                content: "do not run".to_string(),
                created_at: "2026-07-12 12:00:00".to_string(),
                additional_content: Vec::new(),
            });
        plugin
            .cancellation_store
            .record_cancelled(&parent.id, ["delivery-cancelled".to_string()])
            .unwrap();

        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);

        assert!(!plugin
            .team
            .lock()
            .unwrap()
            .registry
            .has_pending_inbox_for(agent_id));
        assert!(plugin
            .cancellation_store
            .load_state(&parent.id)
            .unwrap()
            .cancelled_ids
            .contains("delivery-cancelled"));
    }

    #[test]
    fn restart_restores_durable_internal_delivery_after_restoring_agent() {
        let storage = tempfile::TempDir::new().unwrap();
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let mut parent = Session::new("parent");
        let agent_id = "agent-dev";
        append_agent_history(&mut parent, agent_id);
        plugin
            .cancellation_store
            .record_internal_deliveries(
                &parent.id,
                [(agent_id.to_string(), internal_entry("internal-1", agent_id))],
            )
            .unwrap();

        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);

        let mut team = plugin.team.lock().unwrap();
        assert!(
            team.registry.get(agent_id).is_some(),
            "必须先恢复目标 Agent"
        );
        let inbox = team.registry.drain_inbox(agent_id);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].message.id, "internal-1");
        assert_eq!(inbox[0].message.content, "durable internal work");
        drop(team);
        assert!(plugin
            .cancellation_store
            .load_state(&parent.id)
            .unwrap()
            .pending_internal_deliveries
            .contains_key("internal-1"));
    }

    #[test]
    fn restart_never_restores_cancelled_or_settled_internal_deliveries() {
        let storage = tempfile::TempDir::new().unwrap();
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let mut parent = Session::new("parent");
        let agent_id = "agent-dev";
        append_agent_history(&mut parent, agent_id);
        plugin
            .cancellation_store
            .record_internal_deliveries(
                &parent.id,
                [
                    (
                        agent_id.to_string(),
                        internal_entry("internal-cancelled", agent_id),
                    ),
                    (
                        agent_id.to_string(),
                        internal_entry("internal-settled", agent_id),
                    ),
                ],
            )
            .unwrap();
        plugin
            .cancellation_store
            .record_cancelled(&parent.id, ["internal-cancelled".to_string()])
            .unwrap();
        plugin
            .cancellation_store
            .settle(&parent.id, &["internal-settled".to_string()])
            .unwrap();

        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);

        assert!(!plugin
            .team
            .lock()
            .unwrap()
            .registry
            .has_pending_inbox_for(agent_id));
        let state = plugin.cancellation_store.load_state(&parent.id).unwrap();
        assert!(state.pending_internal_deliveries.is_empty());
        assert!(state.cancelled_ids.contains("internal-cancelled"));
        assert!(state.settled_ids.contains("internal-settled"));
    }

    #[test]
    fn restart_cancels_internal_delivery_for_terminated_target() {
        let storage = tempfile::TempDir::new().unwrap();
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let mut parent = Session::new("parent");
        let agent_id = "agent-dev";
        append_agent_history(&mut parent, agent_id);
        let mut terminated = tiangong_core::session::Message::new(
            tiangong_core::session::MessageRole::System,
            format!("[Agent] Developer 状态变更: terminated id={agent_id}"),
        );
        terminated.model_excluded = true;
        parent.messages.push(terminated);
        plugin
            .cancellation_store
            .record_internal_deliveries(
                &parent.id,
                [(
                    agent_id.to_string(),
                    internal_entry("internal-terminated", agent_id),
                )],
            )
            .unwrap();

        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);

        assert!(plugin.team.lock().unwrap().registry.get(agent_id).is_none());
        let state = plugin.cancellation_store.load_state(&parent.id).unwrap();
        assert!(state.pending_internal_deliveries.is_empty());
        assert!(state.cancelled_ids.contains("internal-terminated"));
    }

    #[test]
    fn restart_with_persisted_receipt_replays_report_without_rerunning_internal_work() {
        let storage = tempfile::TempDir::new().unwrap();
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let mut parent = Session::new("parent");
        let agent_id = "agent-dev";
        append_agent_history(&mut parent, agent_id);
        plugin
            .cancellation_store
            .record_internal_deliveries(
                &parent.id,
                [(agent_id.to_string(), internal_entry("internal-1", agent_id))],
            )
            .unwrap();

        let mut child = Session::new("child");
        child.parent_session_id = Some(parent.id.clone());
        child.active_agent_id = Some(agent_id.to_string());
        let payload = serde_json::json!({
            "delivery_id": "internal-1",
            "report": "already done",
            "main_messages": []
        });
        let mut receipt = tiangong_core::session::Message::new(
            tiangong_core::session::MessageRole::System,
            format!("[agent-team-delivery-receipt]{payload}"),
        );
        receipt.model_excluded = true;
        child.messages.push(receipt);
        crate::lifecycle::persist_child_session_for_parent_id(
            storage.path(),
            &parent.id,
            agent_id,
            &child,
        )
        .unwrap();

        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);

        let team = plugin.team.lock().unwrap();
        assert!(!team.registry.has_pending_inbox_for(agent_id));
        let replays = crate::team_bridge::pending_receipt_replays(&team, &[], &[], &[]);
        assert_eq!(replays.len(), 1);
        assert_eq!(replays[0].delivery_id, "internal-1");
        assert_eq!(replays[0].report, "already done");
    }

    #[test]
    fn restart_fails_closed_when_delivery_protocol_ledger_is_corrupt() {
        let storage = tempfile::TempDir::new().unwrap();
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let agent_id = "agent-dev";
        plugin
            .team
            .lock()
            .unwrap()
            .registry
            .register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "work".to_string(),
                tools: Vec::new(),
                status: AgentStatus::Idle,
            });
        let mut parent = Session::new("parent");
        parent
            .pending_plugin_deliveries
            .push(PendingPluginDelivery {
                delivery_id: "delivery-blocked".to_string(),
                source_message_id: "message-1".to_string(),
                plugin_id: crate::constants::PLUGIN_ID.to_string(),
                target_id: agent_id.to_string(),
                content: "must not run".to_string(),
                created_at: "2026-07-12 12:00:00".to_string(),
                additional_content: Vec::new(),
            });
        let protocol_path = storage
            .path()
            .join("sessions")
            .join(&parent.id)
            .join("agent-team-cancellations.json");
        std::fs::create_dir_all(protocol_path.parent().unwrap()).unwrap();
        std::fs::write(protocol_path, b"not-json").unwrap();

        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);

        assert!(!plugin
            .team
            .lock()
            .unwrap()
            .registry
            .has_pending_inbox_for(agent_id));
        assert_eq!(parent.pending_plugin_deliveries.len(), 1);
    }

    #[test]
    fn restart_moves_parent_completed_ids_into_permanent_settled_ledger() {
        let storage = tempfile::TempDir::new().unwrap();
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        let mut parent = Session::new("parent");
        let agent_id = "agent-dev";
        plugin
            .team
            .lock()
            .unwrap()
            .registry
            .register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "work".to_string(),
                tools: Vec::new(),
                status: AgentStatus::Idle,
            });
        parent.completed_plugin_delivery_ids = vec!["delivery-completed".to_string()];
        parent
            .pending_plugin_deliveries
            .push(PendingPluginDelivery {
                delivery_id: "delivery-completed".to_string(),
                source_message_id: "message-old".to_string(),
                plugin_id: crate::constants::PLUGIN_ID.to_string(),
                target_id: agent_id.to_string(),
                content: "stale snapshot".to_string(),
                created_at: "2026-07-12 12:00:00".to_string(),
                additional_content: Vec::new(),
            });
        plugin
            .cancellation_store
            .record_cancelled(&parent.id, ["delivery-completed".to_string()])
            .unwrap();

        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);

        let state = plugin.cancellation_store.load_state(&parent.id).unwrap();
        assert!(state.cancelled_ids.is_empty());
        assert!(state.settled_ids.contains("delivery-completed"));
        assert!(!plugin
            .team
            .lock()
            .unwrap()
            .registry
            .has_pending_inbox_for(agent_id));
    }

    #[test]
    fn global_cancel_records_and_stops_active_and_queued_work_atomically() {
        let storage = tempfile::TempDir::new().unwrap();
        let plugin = AgentTeamPlugin::new(storage.path().to_path_buf());
        *plugin.session_id.write().unwrap() = Some("session-1".to_string());
        let agent_id = "agent-dev";
        let (command_tx, _command_rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        {
            let mut team = plugin.team.lock().unwrap();
            team.registry.register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "work".to_string(),
                tools: Vec::new(),
                status: AgentStatus::Running,
            });
            team.registry.deliver_inbox_entry(
                agent_id,
                AgentInboxEntry {
                    message: AgentMessage {
                        id: "delivery-queued".to_string(),
                        from: "user".to_string(),
                        to: agent_id.to_string(),
                        content: "queued".to_string(),
                        priority: MessagePriority::Normal,
                        created_at: "2026-07-12 12:00:00".to_string(),
                    },
                    additional_content: Vec::new(),
                    session_message_id: Some("message-queued".to_string()),
                },
            );
            team.register_active_agent(
                agent_id.to_string(),
                command_tx,
                Arc::clone(&cancel_flag),
                Arc::clone(&shutdown_flag),
                Some("delivery-active".to_string()),
            );
        }

        assert!(<AgentTeamPlugin as Plugin>::handle_runtime_command(
            &plugin,
            &Command::Cancel,
        ));

        assert!(cancel_flag.load(Ordering::Acquire));
        assert!(!shutdown_flag.load(Ordering::Acquire));
        assert!(!plugin.team.lock().unwrap().registry.has_pending_inbox());
        let state = plugin.cancellation_store.load_state("session-1").unwrap();
        assert_eq!(
            state.cancelled_ids.into_iter().collect::<Vec<_>>(),
            ["delivery-active", "delivery-queued"]
        );
    }
}
