//! 独立子 Core 的所有权、串行投递与外部事件流终态观察。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tiangong_core::core::plugin::{Plugin, PluginFeedbackTx};
use tiangong_core::core::{CoreStorageLocation, TiangongCore};
use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
use tiangong_core::session::{Message, MessagePhase, MessageRole, Session};
use tiangong_types::{ContentBlock, SessionStreamEvent, StreamEvent, TokenUsage, TurnStatus};

use crate::constants::{CHILD_PLUGIN_ID, PLUGIN_ID};
use crate::state::AgentDescriptor;

/// 每次为一个子 Core 创建全新的插件 facade。
pub trait ChildPluginFactory: Send + Sync {
    fn create_plugins(&self) -> Vec<Arc<dyn Plugin>>;
}

impl<F> ChildPluginFactory for F
where
    F: Fn() -> Vec<Arc<dyn Plugin>> + Send + Sync,
{
    fn create_plugins(&self) -> Vec<Arc<dyn Plugin>> {
        self()
    }
}

pub(crate) type SharedFeedback = Arc<RwLock<Option<PluginFeedbackTx>>>;

#[derive(Debug, Clone)]
pub(crate) struct ChildTurnResult {
    pub status: TurnStatus,
    pub error: Option<String>,
    pub usage: TokenUsage,
    pub assistant_text: Option<String>,
}

pub(crate) struct ChildRuntime {
    descriptor: AgentDescriptor,
    core: Mutex<Option<TiangongCore>>,
    /// 子 Agent 会话存储根目录，用于在工作目录变更时直接更新磁盘 session。
    storage_root: PathBuf,
    /// 串行化“进入 Running + waiter 登记 + Message 入队”与 Cancel 入队。
    command_gate: Mutex<()>,
    delivery_gate: tokio::sync::Mutex<()>,
    shutdown_gate: tokio::sync::Mutex<()>,
    state: Arc<Mutex<RuntimeState>>,
    active_turn: Arc<Mutex<Option<ActiveTurn>>>,
    event_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    completed_messages: Arc<Mutex<HashMap<String, ChildTurnResult>>>,
}

impl ChildRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        descriptor: AgentDescriptor,
        mut session: Session,
        config: CoreConfig,
        child_storage_root: PathBuf,
        core_storage_root: PathBuf,
        mut plugins: Vec<Arc<dyn Plugin>>,
        team_client: Arc<dyn Plugin>,
        base_feedback: SharedFeedback,
    ) -> Result<Arc<Self>, String> {
        if session.id != descriptor.agent_id {
            return Err(format!(
                "子 Core 会话 ID 与团队成员 ID 不一致：{} != {}",
                session.id, descriptor.agent_id
            ));
        }
        let completed_messages = Arc::new(Mutex::new(recover_completed_messages(&session)));
        session.bind_storage_root(child_storage_root.clone());
        session.try_persist_to_disk()?;

        plugins.retain(|plugin| plugin.id() != PLUGIN_ID && plugin.id() != CHILD_PLUGIN_ID);
        plugins.push(team_client);

        let (event_tx, event_rx) = std::sync::mpsc::channel::<SessionStreamEvent>();
        let active_turn = Arc::new(Mutex::new(None));
        let state = Arc::new(Mutex::new(RuntimeState::Idle));
        let event_thread = spawn_event_bridge(
            descriptor.clone(),
            Arc::clone(&active_turn),
            Arc::clone(&state),
            Arc::clone(&completed_messages),
            base_feedback,
            event_rx,
        );
        let core = match TiangongCore::builder()
            .config(CoreConfigProvider::new(config))
            .session(session)
            .event_sender(event_tx)
            .plugins(plugins)
            .storage(CoreStorageLocation::new(core_storage_root))
            .build()
        {
            Ok(core) => core,
            Err(error) => {
                let _ = event_thread.join();
                return Err(format!("构造子 Agent Core 失败：{error}"));
            }
        };

        Ok(Arc::new(Self {
            descriptor,
            core: Mutex::new(Some(core)),
            storage_root: child_storage_root,
            command_gate: Mutex::new(()),
            delivery_gate: tokio::sync::Mutex::new(()),
            shutdown_gate: tokio::sync::Mutex::new(()),
            state,
            active_turn,
            event_thread: Mutex::new(Some(event_thread)),
            completed_messages,
        }))
    }

    pub(crate) fn descriptor(&self) -> &AgentDescriptor {
        &self.descriptor
    }

    pub(crate) fn is_running(&self) -> bool {
        self.state
            .lock()
            .map(|state| *state == RuntimeState::Running)
            .unwrap_or_else(|poison| *poison.into_inner() == RuntimeState::Running)
    }

    pub(crate) fn begin_closing(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if *state != RuntimeState::Idle {
            return false;
        }
        *state = RuntimeState::Closing;
        true
    }

    pub(crate) fn reopen(&self) {
        if let Ok(mut state) = self.state.lock() {
            if *state == RuntimeState::Closing {
                *state = RuntimeState::Idle;
            }
        }
    }

    pub(crate) fn prepare_shutdown(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = RuntimeState::Closing;
        }
        let _ = self.cancel();
    }

    pub(crate) async fn deliver_and_wait(
        &self,
        message_id: String,
        prepared: Vec<ContentBlock>,
        feedback: PluginFeedbackTx,
    ) -> Result<ChildTurnResult, String> {
        let _delivery_guard = self.delivery_gate.lock().await;
        let command_guard = self
            .command_gate
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _running_guard = RunningGuard::try_new(&self.state, &self.active_turn)?;
        if let Some(result) = self
            .completed_messages
            .lock()
            .ok()
            .and_then(|completed| completed.get(&message_id).cloned())
        {
            return Ok(result);
        }

        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        {
            let mut active = self
                .active_turn
                .lock()
                .map_err(|_| "子 Agent 终态观察器锁定失败".to_string())?;
            if active.is_some() {
                return Err("子 Agent 已有未完成的事件流等待器".to_string());
            }
            *active = Some(ActiveTurn {
                message_id: message_id.clone(),
                started: false,
                feedback,
                terminal_tx,
                collector: TurnCollector::default(),
            });
        }

        // fire-and-forget：deliver 后不等持久化确认。worker 在消息持久化失败时会
        // 发出 Error 终态，由下方 terminal_rx 的 Err 路径接管。
        if let Err(error) = (|| {
            let core = self
                .core
                .lock()
                .map_err(|_| "子 Agent Core 状态锁定失败".to_string())?;
            core.as_ref()
                .ok_or_else(|| "子 Agent Core 已关闭".to_string())?
                .deliver(
                    tiangong_core::agent_input::AgentInputKind::prepared_with_id(
                        message_id.clone(),
                        prepared,
                    ),
                )
                .map_err(|error| format!("投递子 Agent 消息失败：{error}"))
        })() {
            self.remove_active_turn(&message_id);
            return Err(error);
        }
        // Message 已经与 Running/waiter 一起原子排在任何后续 Cancel 之前；异步
        // 等待终态时不持有同步锁。
        drop(command_guard);

        let result = terminal_rx
            .await
            .map_err(|_| "子 Agent 外部事件流在终态前关闭".to_string())??;
        Ok(result)
    }

    fn remove_active_turn(&self, message_id: &str) {
        if let Ok(mut active) = self.active_turn.lock() {
            if active
                .as_ref()
                .is_some_and(|turn| turn.message_id == message_id)
            {
                *active = None;
            }
        }
    }

    pub(crate) fn cancel(&self) -> bool {
        let _command_guard = self
            .command_gate
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.enqueue_cancel()
    }

    pub(crate) fn cancel_if_active(&self, message_id: &str) -> bool {
        let _command_guard = self
            .command_gate
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let matches = self
            .active_turn
            .lock()
            .ok()
            .and_then(|active| active.as_ref().map(|turn| turn.message_id == message_id))
            .unwrap_or(false);
        matches && self.enqueue_cancel()
    }

    fn enqueue_cancel(&self) -> bool {
        self.core
            .lock()
            .ok()
            .and_then(|core| {
                core.as_ref()
                    .map(|core| core.deliver(AgentInputKind::cancel()).is_ok())
            })
            .unwrap_or(false)
    }

    pub(crate) fn replace_base_config(&self, base: &CoreConfig) -> Result<(), String> {
        let config = child_config(base, &self.descriptor);
        let core = self
            .core
            .lock()
            .map_err(|_| "子 Agent Core 状态锁定失败".to_string())?;
        core.as_ref()
            .ok_or_else(|| "子 Agent Core 已关闭".to_string())?
            .replace_config(config)
            .map_err(|error| format!("更新子 Agent 配置失败：{error}"))
    }

    pub(crate) fn update_workspace(&self, workspace: Option<&Path>) -> Result<(), String> {
        let cwd = workspace
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        // 工作目录变更直接更新磁盘 session；下次 turn 从磁盘重载 cwd。
        let mut session = Session::load_from_storage(&self.storage_root, &self.descriptor.agent_id)
            .map_err(|error| format!("加载子 Agent 会话失败：{error}"))?;
        session.cwd = cwd;
        session
            .try_persist_to_disk()
            .map_err(|error| format!("持久化子 Agent 工作目录失败：{error}"))
    }

    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        let _shutdown_guard = self.shutdown_gate.lock().await;
        if let Ok(mut state) = self.state.lock() {
            *state = RuntimeState::Closing;
        }
        let _ = self.cancel();
        let core = self
            .core
            .lock()
            .map_err(|_| "子 Agent Core 状态锁定失败".to_string())?
            .take();
        let event_thread = self
            .event_thread
            .lock()
            .map_err(|_| "子 Agent 事件线程状态锁定失败".to_string())?
            .take();
        let Some(core) = core else {
            if let Some(thread) = event_thread {
                let _ = thread.join();
            }
            return Ok(());
        };
        tokio::task::spawn_blocking(move || {
            let result = core
                .shutdown_join()
                .map_err(|error| format!("关闭子 Agent Core 失败：{error}"));
            if let Some(thread) = event_thread {
                thread
                    .join()
                    .map_err(|_| "子 Agent 事件转发线程异常退出".to_string())?;
            }
            result
        })
        .await
        .map_err(|error| format!("等待子 Agent Core 关闭任务失败：{error}"))?
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeState {
    Idle,
    Running,
    Closing,
}

struct RunningGuard<'a> {
    state: &'a Mutex<RuntimeState>,
    active_turn: &'a Mutex<Option<ActiveTurn>>,
}

impl<'a> RunningGuard<'a> {
    fn try_new(
        state: &'a Mutex<RuntimeState>,
        active_turn: &'a Mutex<Option<ActiveTurn>>,
    ) -> Result<Self, String> {
        let active = active_turn
            .lock()
            .map_err(|_| "子 Agent 终态观察器锁定失败".to_string())?;
        if active.is_some() {
            return Err("子 Agent 已有未完成的事件流等待器".to_string());
        }
        let mut current = state
            .lock()
            .map_err(|_| "子 Agent 运行状态锁定失败".to_string())?;
        if *current == RuntimeState::Closing {
            return Err("子 Agent Core 正在关闭，拒绝新投递".to_string());
        }
        *current = RuntimeState::Running;
        drop(current);
        drop(active);
        Ok(Self { state, active_turn })
    }
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        // Future 被取消时 active waiter 仍在，子 Core 仍可能执行；此时不能把状态
        // 提前改为空闲。锁顺序固定为 active_turn → state。
        if let Ok(active) = self.active_turn.lock() {
            if active.is_none() {
                if let Ok(mut state) = self.state.lock() {
                    if *state == RuntimeState::Running {
                        *state = RuntimeState::Idle;
                    }
                }
            }
        }
    }
}

struct ActiveTurn {
    message_id: String,
    /// 只有观察到当前稳定消息 ID 的 UserMessage 后，后续终态才属于本轮。
    started: bool,
    feedback: PluginFeedbackTx,
    terminal_tx: tokio::sync::oneshot::Sender<Result<ChildTurnResult, String>>,
    collector: TurnCollector,
}

#[derive(Default)]
struct TurnCollector {
    usage: UsageAccumulator,
    summary_text: String,
    delta_text: String,
    react_text: String,
    persisted_assistant_text: Option<String>,
    streamed_messages: HashMap<String, StreamedMessage>,
}

impl TurnCollector {
    fn append_text(
        &mut self,
        source_message_id: &str,
        output_message_id: String,
        content: &str,
        phase: MessagePhase,
        kind: TextKind,
    ) -> Message {
        match kind {
            TextKind::Summary => self.summary_text.push_str(content),
            TextKind::Delta => self.delta_text.push_str(content),
            TextKind::React => self.react_text.push_str(content),
        }
        let stream = self
            .streamed_messages
            .entry(source_message_id.to_string())
            .or_default();
        stream.content.push_str(content);
        stream.phase = phase;
        let mut message = Message::new(MessageRole::Assistant, content).with_phase(phase);
        message.id = output_message_id;
        message
    }

    fn append_reasoning(
        &mut self,
        source_message_id: &str,
        output_message_id: String,
        content: &str,
    ) -> Message {
        let stream = self
            .streamed_messages
            .entry(source_message_id.to_string())
            .or_default();
        stream.reasoning.push_str(content);
        let mut message = Message::with_reasoning(MessageRole::Assistant, String::new(), content);
        message.id = output_message_id;
        message
    }

    fn observe_persisted_message(&mut self, message: &Message) {
        if message.role == MessageRole::Assistant {
            let text = message.text_content();
            if !text.trim().is_empty() {
                self.persisted_assistant_text = Some(text);
            }
        }
    }

    fn snapshot_delta(&mut self, message: &Message) -> Option<Message> {
        self.observe_persisted_message(message);
        let full_content = message.text_content();
        let full_reasoning = message.reasoning_content.clone();
        let stream = match self.streamed_messages.entry(message.id.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(StreamedMessage {
                    content: full_content,
                    reasoning: full_reasoning,
                    phase: message.phase,
                });
                return Some(message.clone());
            }
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        };
        let content_delta = full_content
            .strip_prefix(&stream.content)
            .unwrap_or_default()
            .to_string();
        let reasoning_delta = full_reasoning
            .strip_prefix(&stream.reasoning)
            .unwrap_or_default()
            .to_string();
        let phase_changed = stream.phase != message.phase;
        if full_content.starts_with(&stream.content) {
            stream.content = full_content;
        }
        if full_reasoning.starts_with(&stream.reasoning) {
            stream.reasoning = full_reasoning;
        }
        stream.phase = message.phase;
        if content_delta.is_empty() && reasoning_delta.is_empty() && !phase_changed {
            return None;
        }
        let mut delta =
            Message::with_reasoning(MessageRole::Assistant, content_delta, reasoning_delta);
        delta.phase = message.phase;
        Some(delta)
    }

    fn assistant_text(&self) -> Option<String> {
        self.persisted_assistant_text.clone().or_else(|| {
            [&self.summary_text, &self.delta_text, &self.react_text]
                .into_iter()
                .find(|text| !text.trim().is_empty())
                .cloned()
        })
    }

    fn finish(self, terminal: StreamEvent) -> ChildTurnResult {
        let assistant_text = self.assistant_text();
        match terminal {
            StreamEvent::Done { usage } => ChildTurnResult {
                status: TurnStatus::Success,
                error: None,
                usage: usage.unwrap_or_else(|| self.usage.total()),
                assistant_text,
            },
            StreamEvent::Error { message } => {
                let lower = message.to_lowercase();
                ChildTurnResult {
                    status: if message.contains("取消")
                        || message.contains("中断")
                        || lower.contains("cancel")
                        || lower.contains("abort")
                    {
                        TurnStatus::Cancelled
                    } else {
                        TurnStatus::Failed
                    },
                    error: Some(message),
                    usage: self.usage.total(),
                    assistant_text,
                }
            }
            _ => unreachable!("finish 只接收终态"),
        }
    }
}

#[derive(Default)]
struct StreamedMessage {
    content: String,
    reasoning: String,
    phase: MessagePhase,
}

#[derive(Clone, Copy)]
enum TextKind {
    Summary,
    Delta,
    React,
}

#[derive(Default)]
struct UsageAccumulator {
    total: TokenUsage,
    observed: TokenUsage,
}

impl UsageAccumulator {
    fn observe(&mut self, usage: &TokenUsage, source: &str) {
        let mut usage = usage.clone();
        if usage.total_tokens == 0 {
            usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
        }
        if source == "cancelled-cumulative" {
            let delta = usage_delta(&usage, &self.observed);
            merge_cumulative(&mut self.observed, &usage);
            self.total.accumulate(&delta);
        } else {
            self.observed.accumulate(&usage);
            self.total.accumulate(&usage);
        }
    }

    fn total(&self) -> TokenUsage {
        self.total.clone()
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

fn merge_cumulative(observed: &mut TokenUsage, cumulative: &TokenUsage) {
    observed.prompt_tokens = observed.prompt_tokens.max(cumulative.prompt_tokens);
    observed.completion_tokens = observed.completion_tokens.max(cumulative.completion_tokens);
    observed.total_tokens = observed
        .total_tokens
        .max(cumulative.total_tokens)
        .max(observed.prompt_tokens + observed.completion_tokens);
    observed.prompt_cache_hit_tokens = max_optional(
        observed.prompt_cache_hit_tokens,
        cumulative.prompt_cache_hit_tokens,
    );
    observed.prompt_cache_miss_tokens = max_optional(
        observed.prompt_cache_miss_tokens,
        cumulative.prompt_cache_miss_tokens,
    );
}

fn max_optional(current: Option<usize>, cumulative: Option<usize>) -> Option<usize> {
    match (current, cumulative) {
        (Some(current), Some(cumulative)) => Some(current.max(cumulative)),
        (current, cumulative) => current.or(cumulative),
    }
}

fn spawn_event_bridge(
    descriptor: AgentDescriptor,
    active_turn: Arc<Mutex<Option<ActiveTurn>>>,
    state: Arc<Mutex<RuntimeState>>,
    completed_messages: Arc<Mutex<HashMap<String, ChildTurnResult>>>,
    base_feedback: SharedFeedback,
    event_rx: std::sync::mpsc::Receiver<SessionStreamEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            handle_child_event(
                &descriptor,
                &active_turn,
                &state,
                &completed_messages,
                &base_feedback,
                event.event,
            );
        }
        // 锁顺序固定为 active_turn → state。通道关闭后即使 waiter 已被调用方
        // 取消，也必须进入 Closing，禁止 dismiss 把仍未知的 Core 状态当成空闲。
        let pending = if let Ok(mut active) = active_turn.lock() {
            let pending = active.take();
            if let Ok(mut state) = state.lock() {
                *state = RuntimeState::Closing;
            }
            pending
        } else {
            None
        };
        if let Some(turn) = pending {
            forward_status(&turn.feedback, &descriptor, "idle");
            let _ = turn
                .terminal_tx
                .send(Err("子 Agent 外部事件通道已关闭".to_string()));
        }
    })
}

fn handle_child_event(
    descriptor: &AgentDescriptor,
    active_turn: &Mutex<Option<ActiveTurn>>,
    state: &Mutex<RuntimeState>,
    completed_messages: &Mutex<HashMap<String, ChildTurnResult>>,
    base_feedback: &SharedFeedback,
    event: StreamEvent,
) {
    match event {
        terminal @ (StreamEvent::Done { .. } | StreamEvent::Error { .. }) => {
            // 匹配终态是真正的运行边界。先取 waiter，再在同一锁序下把状态改回
            // Idle，避免等待 Future 被取消时提前允许 dismiss。
            let turn = if let Ok(mut active) = active_turn.lock() {
                if active.as_ref().is_some_and(|turn| turn.started) {
                    let turn = active.take();
                    if let Ok(mut state) = state.lock() {
                        if *state == RuntimeState::Running {
                            *state = RuntimeState::Idle;
                        }
                    }
                    turn
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(turn) = turn {
                let result = turn.collector.finish(terminal);
                if let Ok(mut completed) = completed_messages.lock() {
                    let mut replay = result.clone();
                    replay.usage = TokenUsage::default();
                    completed.insert(turn.message_id.clone(), replay);
                }
                forward_status(&turn.feedback, descriptor, "idle");
                let _ = turn.terminal_tx.send(Ok(result));
            }
        }
        StreamEvent::TokenUsage {
            usage,
            current_tokens,
            compression_threshold_tokens,
            context_limit_tokens,
            source,
            agent_id,
        } => {
            let feedback = if let Ok(mut active) = active_turn.lock() {
                if let Some(turn) = active.as_mut().filter(|turn| turn.started) {
                    turn.collector.usage.observe(&usage, &source);
                    Some(turn.feedback.clone())
                } else {
                    None
                }
            } else {
                None
            }
            .or_else(|| shared_feedback(base_feedback));
            if let Some(feedback) = feedback {
                forward_event(
                    &feedback,
                    StreamEvent::TokenUsage {
                        usage,
                        current_tokens,
                        compression_threshold_tokens,
                        context_limit_tokens,
                        source,
                        agent_id: agent_id.or_else(|| Some(descriptor.agent_id.clone())),
                    },
                );
            }
        }
        StreamEvent::ToolCalls {
            message_id, names, ..
        } => forward_system_output(
            descriptor,
            active_turn,
            base_feedback,
            "tool-calls",
            format!("{message_id}:{}", scru128::new()),
            format!("tool_calls: {}", names.join(", ")),
        ),
        StreamEvent::ToolStart { name, args_summary } => forward_system_output(
            descriptor,
            active_turn,
            base_feedback,
            "tool-start",
            scru128::new().to_string(),
            if args_summary.is_empty() {
                format!("工具开始 [{name}]")
            } else {
                format!("工具开始 [{name}]\n命令: {args_summary}")
            },
        ),
        StreamEvent::ToolResult {
            name,
            tool_call_id,
            ok,
            output,
            full_output,
            ..
        } => forward_system_output(
            descriptor,
            active_turn,
            base_feedback,
            "tool-result",
            format!(
                "{}:{}",
                tool_call_id.unwrap_or_else(|| "unknown".to_string()),
                scru128::new()
            ),
            format!(
                "工具执行 [{name}]\nok={ok}\n{}",
                full_output.unwrap_or(output)
            ),
        ),
        StreamEvent::Retry { message, .. } => {
            if let Some(feedback) = feedback_for_event(active_turn, base_feedback) {
                forward_event(
                    &feedback,
                    StreamEvent::AgentNotification {
                        agent_id: descriptor.agent_id.clone(),
                        agent_label: descriptor.label.clone(),
                        content: message,
                        level: "warning".to_string(),
                    },
                );
            }
        }
        StreamEvent::PhaseChanged { .. } => {}
        StreamEvent::Delta {
            message_id,
            content,
        } => forward_text_event(
            descriptor,
            active_turn,
            base_feedback,
            &message_id,
            &content,
            MessagePhase::Normal,
            TextKind::Delta,
        ),
        StreamEvent::ReactText {
            message_id,
            content,
        } => forward_text_event(
            descriptor,
            active_turn,
            base_feedback,
            &message_id,
            &content,
            MessagePhase::React,
            TextKind::React,
        ),
        StreamEvent::SummaryText {
            message_id,
            content,
        } => forward_text_event(
            descriptor,
            active_turn,
            base_feedback,
            &message_id,
            &content,
            MessagePhase::Summary,
            TextKind::Summary,
        ),
        StreamEvent::Reasoning {
            message_id,
            content,
        } => {
            if content.is_empty() {
                return;
            }
            let output_message_id = namespaced_message_id(descriptor, "assistant", &message_id);
            let pair = if let Ok(mut active) = active_turn.lock() {
                active.as_mut().filter(|turn| turn.started).map(|turn| {
                    (
                        turn.feedback.clone(),
                        turn.collector.append_reasoning(
                            &message_id,
                            output_message_id.clone(),
                            &content,
                        ),
                    )
                })
            } else {
                None
            }
            .or_else(|| {
                shared_feedback(base_feedback).map(|feedback| {
                    let mut message =
                        Message::with_reasoning(MessageRole::Assistant, String::new(), content);
                    message.id = output_message_id;
                    (feedback, message)
                })
            });
            let Some((feedback, message)) = pair else {
                return;
            };
            forward_agent_output(&feedback, descriptor, vec![message]);
        }
        StreamEvent::SessionMessageUpsert { message, .. } => {
            if message.role != MessageRole::Assistant {
                return;
            }
            let source_message_id = message.id.clone();
            let delivery = match active_turn.lock() {
                Ok(mut active) => {
                    if let Some(turn) = active.as_mut().filter(|turn| turn.started) {
                        turn.collector
                            .snapshot_delta(&message)
                            .map(|message| (turn.feedback.clone(), message))
                    } else {
                        shared_feedback(base_feedback).map(|feedback| (feedback, message.clone()))
                    }
                }
                Err(_) => {
                    shared_feedback(base_feedback).map(|feedback| (feedback, message.clone()))
                }
            };
            if let Some((feedback, mut message)) = delivery {
                message.id = namespaced_message_id(descriptor, "assistant", &source_message_id);
                forward_agent_output(&feedback, descriptor, vec![message]);
            }
        }
        StreamEvent::ApprovalNeeded {
            request_id,
            tool_name,
            args_summary,
        } => {
            if let Some(feedback) = feedback_for_event(active_turn, base_feedback) {
                forward_event(
                    &feedback,
                    StreamEvent::ApprovalNeeded {
                        request_id,
                        tool_name,
                        args_summary,
                    },
                );
            }
        }
        StreamEvent::UserMessage { message_id, .. } => {
            let feedback = if let Ok(mut active) = active_turn.lock() {
                if let Some(turn) = active.as_mut() {
                    if turn.message_id == message_id && !turn.started {
                        turn.started = true;
                        Some(turn.feedback.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(feedback) = feedback {
                forward_status(&feedback, descriptor, "running");
            }
        }
        StreamEvent::DeferredToolInjectionsChanged { .. } | StreamEvent::TurnBoundary { .. } => {}
        other => {
            if let Some(feedback) = feedback_for_event(active_turn, base_feedback) {
                forward_event(&feedback, other);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn forward_text_event(
    descriptor: &AgentDescriptor,
    active_turn: &Mutex<Option<ActiveTurn>>,
    base_feedback: &SharedFeedback,
    message_id: &str,
    content: &str,
    phase: MessagePhase,
    kind: TextKind,
) {
    if content.is_empty() {
        return;
    }
    let output_message_id = namespaced_message_id(descriptor, "assistant", message_id);
    let pair = if let Ok(mut active) = active_turn.lock() {
        active.as_mut().filter(|turn| turn.started).map(|turn| {
            (
                turn.feedback.clone(),
                turn.collector.append_text(
                    message_id,
                    output_message_id.clone(),
                    content,
                    phase,
                    kind,
                ),
            )
        })
    } else {
        None
    };
    if let Some((feedback, message)) = pair {
        forward_agent_output(&feedback, descriptor, vec![message]);
    } else if let Some(feedback) = shared_feedback(base_feedback) {
        let mut message = Message::new(MessageRole::Assistant, content).with_phase(phase);
        message.id = output_message_id;
        forward_agent_output(&feedback, descriptor, vec![message]);
    }
}

fn feedback_for_event(
    active_turn: &Mutex<Option<ActiveTurn>>,
    base_feedback: &SharedFeedback,
) -> Option<PluginFeedbackTx> {
    active_turn
        .lock()
        .ok()
        .and_then(|active| {
            active
                .as_ref()
                .filter(|turn| turn.started)
                .map(|turn| turn.feedback.clone())
        })
        .or_else(|| shared_feedback(base_feedback))
}

fn shared_feedback(feedback: &SharedFeedback) -> Option<PluginFeedbackTx> {
    feedback.read().ok().and_then(|feedback| feedback.clone())
}

fn forward_agent_output(
    feedback: &PluginFeedbackTx,
    descriptor: &AgentDescriptor,
    messages: Vec<Message>,
) {
    forward_event(
        feedback,
        StreamEvent::AgentOutput {
            agent_id: descriptor.agent_id.clone(),
            agent_role: descriptor.role.clone(),
            agent_label: descriptor.label.clone(),
            messages,
        },
    );
}

fn forward_system_output(
    descriptor: &AgentDescriptor,
    active_turn: &Mutex<Option<ActiveTurn>>,
    base_feedback: &SharedFeedback,
    namespace: &str,
    source_message_id: String,
    content: String,
) {
    let Some(feedback) = feedback_for_event(active_turn, base_feedback) else {
        return;
    };
    let mut message = Message::new(MessageRole::System, content);
    message.id = namespaced_message_id(descriptor, namespace, &source_message_id);
    forward_agent_output(&feedback, descriptor, vec![message]);
}

fn namespaced_message_id(
    descriptor: &AgentDescriptor,
    namespace: &str,
    source_message_id: &str,
) -> String {
    format!(
        "agent:{}:{namespace}:{source_message_id}",
        descriptor.agent_id
    )
}

fn forward_event(feedback: &PluginFeedbackTx, event: StreamEvent) {
    if !feedback.send_turn_stream_event(event.clone()) {
        feedback.send_stream_event(event);
    }
}

fn forward_status(feedback: &PluginFeedbackTx, descriptor: &AgentDescriptor, status: &str) {
    forward_event(
        feedback,
        StreamEvent::AgentStatusChanged {
            agent_id: descriptor.agent_id.clone(),
            label: descriptor.label.clone(),
            status: status.to_string(),
        },
    );
}

fn recover_completed_messages(session: &Session) -> HashMap<String, ChildTurnResult> {
    let mut completed = HashMap::new();
    for (index, message) in session.messages.iter().enumerate() {
        if message.role != MessageRole::User {
            continue;
        }
        let Some(status) = message.turn_status else {
            continue;
        };
        let tail = &session.messages[index + 1..];
        let turn_end = tail
            .iter()
            .position(|next| next.role == MessageRole::User)
            .unwrap_or(tail.len());
        let assistant_text = tail[..turn_end]
            .iter()
            .rev()
            .find(|next| next.role == MessageRole::Assistant)
            .map(Message::text_content)
            .filter(|text| !text.trim().is_empty());
        completed.insert(
            message.id.clone(),
            ChildTurnResult {
                status,
                error: (status != TurnStatus::Success)
                    .then(|| "从已持久化的子 Agent 轮次恢复".to_string()),
                usage: TokenUsage::default(),
                assistant_text,
            },
        );
    }
    completed
}

pub(crate) fn child_config(base: &CoreConfig, descriptor: &AgentDescriptor) -> CoreConfig {
    let mut config = base.clone();
    config.trust_mode = tiangong_core::permission::TrustMode::FullTrust;
    config.default_trust_mode = tiangong_core::permission::TrustMode::FullTrust;
    let inherited = config.custom_system_prompt.trim();
    let role_prompt = format!(
        "你是团队成员 {}（@{}，Session ID={}）。\n{}",
        descriptor.label,
        descriptor.role,
        descriptor.agent_id,
        descriptor.system_prompt.trim()
    );
    config.custom_system_prompt = if inherited.is_empty() {
        role_prompt
    } else {
        format!("{inherited}\n\n{role_prompt}")
    };
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use tiangong_core::core::Plugin;
    use tiangong_core::core_config::ModelEndpoint;
    use tiangong_core::tool_override::{
        PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
    };

    #[derive(Default)]
    struct EmptyPlugin;

    impl ToolOverrideHandler for EmptyPlugin {}
    impl ToolSpecProvider for EmptyPlugin {}
    impl PromptSectionProvider for EmptyPlugin {}

    impl Plugin for EmptyPlugin {
        fn id(&self) -> &str {
            "child-runtime-test-empty"
        }
    }

    #[derive(Default)]
    struct FeedbackCapturePlugin {
        feedback: Arc<Mutex<Option<PluginFeedbackTx>>>,
    }

    impl ToolOverrideHandler for FeedbackCapturePlugin {}
    impl ToolSpecProvider for FeedbackCapturePlugin {}
    impl PromptSectionProvider for FeedbackCapturePlugin {}

    impl Plugin for FeedbackCapturePlugin {
        fn id(&self) -> &str {
            "child-runtime-test-feedback"
        }

        fn set_feedback_tx(&self, feedback: PluginFeedbackTx) {
            *self.feedback.lock().unwrap() = Some(feedback);
        }
    }

    struct OneShotSseServer {
        base_url: String,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl OneShotSseServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let thread = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 2048];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                let payload = serde_json::json!({
                    "id": "chatcmpl-child-runtime",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "test-model",
                    "choices": [{
                        "index": 0,
                        "delta": { "content": "子 Agent 已完成" },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 11,
                        "completion_tokens": 4,
                        "total_tokens": 15
                    }
                });
                let body = format!("data: {payload}\n\ndata: [DONE]\n\n");
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            });
            Self {
                base_url: format!("http://{address}/v1"),
                thread: Some(thread),
            }
        }
    }

    impl Drop for OneShotSseServer {
        fn drop(&mut self) {
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    /// 接受连接但不返回模型数据；用于验证 Cancel 与 Message 的入队顺序。
    struct StallingServer {
        base_url: String,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl StallingServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                let mut connection = None;
                while !thread_stop.load(Ordering::Acquire) {
                    if connection.is_none() {
                        match listener.accept() {
                            Ok((stream, _)) => connection = Some(stream),
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                            Err(_) => break,
                        }
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                drop(connection);
            });
            Self {
                base_url: format!("http://{address}/v1"),
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for StallingServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    fn usage(prompt: usize, completion: usize) -> TokenUsage {
        TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }
    }

    #[test]
    fn done_usage_is_authoritative_and_summary_has_text_priority() {
        let mut collector = TurnCollector::default();
        collector.append_text(
            "delta",
            "agent:test:assistant:delta".to_string(),
            "普通文本",
            MessagePhase::Normal,
            TextKind::Delta,
        );
        collector.append_text(
            "summary",
            "agent:test:assistant:summary".to_string(),
            "最终总结",
            MessagePhase::Summary,
            TextKind::Summary,
        );
        collector.usage.observe(&usage(10, 2), "streaming");

        let result = collector.finish(StreamEvent::Done {
            usage: Some(usage(20, 5)),
        });
        assert_eq!(result.status, TurnStatus::Success);
        assert_eq!(result.assistant_text.as_deref(), Some("最终总结"));
        assert_eq!(result.usage.total_tokens, 25);
    }

    #[test]
    fn streamed_agent_output_uses_incremental_chunks_and_namespaced_ids() {
        let descriptor = AgentDescriptor {
            agent_id: "child-session".to_string(),
            role: "dev".to_string(),
            label: "Developer".to_string(),
            system_prompt: "work".to_string(),
            status: crate::state::AgentStatus::Idle,
        };
        let mut collector = TurnCollector::default();
        let output_id = namespaced_message_id(&descriptor, "assistant", "assistant-1");
        let first = collector.append_text(
            "assistant-1",
            output_id.clone(),
            "第一段",
            MessagePhase::Normal,
            TextKind::Delta,
        );
        let second = collector.append_text(
            "assistant-1",
            output_id.clone(),
            "第二段",
            MessagePhase::Normal,
            TextKind::Delta,
        );
        let reasoning = collector.append_reasoning(
            "assistant-1",
            namespaced_message_id(&descriptor, "assistant", "assistant-1"),
            "思考片段",
        );

        assert_eq!(first.id, "agent:child-session:assistant:assistant-1");
        assert_eq!(first.text_content(), "第一段");
        assert_eq!(second.id, output_id);
        assert_eq!(second.text_content(), "第二段");
        assert_eq!(reasoning.id, "agent:child-session:assistant:assistant-1");
        assert_eq!(reasoning.reasoning_content, "思考片段");
        assert_eq!(
            namespaced_message_id(&descriptor, "tool-calls", "assistant-1"),
            "agent:child-session:tool-calls:assistant-1"
        );
        assert_eq!(collector.delta_text, "第一段第二段");
        assert_eq!(
            collector
                .streamed_messages
                .get("assistant-1")
                .map(|message| message.content.as_str()),
            Some("第一段第二段")
        );
    }

    #[test]
    fn persisted_snapshot_only_forwards_missing_components_and_phase() {
        let mut snapshot = Message::new(MessageRole::Assistant, "最终快照");
        snapshot.id = "assistant-1".to_string();

        let mut fallback_collector = TurnCollector::default();
        assert_eq!(
            fallback_collector
                .snapshot_delta(&snapshot)
                .map(|message| message.text_content()),
            Some("最终快照".to_string())
        );
        assert!(fallback_collector.snapshot_delta(&snapshot).is_none());
        snapshot.phase = MessagePhase::Summary;
        let phase_update = fallback_collector
            .snapshot_delta(&snapshot)
            .expect("最终阶段变化仍应同步");
        assert!(phase_update.text_content().is_empty());
        assert_eq!(phase_update.phase, MessagePhase::Summary);

        let mut streamed_collector = TurnCollector::default();
        streamed_collector.append_text(
            "assistant-1",
            "agent:child-session:assistant:assistant-1".to_string(),
            "流式",
            MessagePhase::Normal,
            TextKind::Delta,
        );
        let mut completed = Message::with_reasoning(MessageRole::Assistant, "流式文本", "完整思考");
        completed.id = "assistant-1".to_string();
        let missing = streamed_collector
            .snapshot_delta(&completed)
            .expect("快照应补齐未流式的文本和思考");
        assert_eq!(missing.text_content(), "文本");
        assert_eq!(missing.reasoning_content, "完整思考");
        assert_eq!(
            streamed_collector.persisted_assistant_text.as_deref(),
            Some("流式文本")
        );
        assert_eq!(
            streamed_collector.assistant_text().as_deref(),
            Some("流式文本")
        );
    }

    #[test]
    fn cancelled_cumulative_usage_only_adds_unseen_delta() {
        let mut collector = UsageAccumulator::default();
        collector.observe(&usage(10, 2), "streaming");
        collector.observe(&usage(15, 5), "cancelled-cumulative");
        let total = collector.total();
        assert_eq!(total.prompt_tokens, 15);
        assert_eq!(total.completion_tokens, 5);
        assert_eq!(total.total_tokens, 20);
    }

    #[test]
    fn recovered_completed_turn_is_replayable_without_usage() {
        let mut session = Session::new("child");
        let mut user = Message::new(MessageRole::User, "work");
        user.id = "delivery-1".to_string();
        user.set_turn_result(10, TurnStatus::Success);
        session.messages.push(user);
        session
            .messages
            .push(Message::new(MessageRole::Assistant, "done"));

        let recovered = recover_completed_messages(&session);
        let result = recovered.get("delivery-1").unwrap();
        assert_eq!(result.assistant_text.as_deref(), Some("done"));
        assert_eq!(result.usage.total_tokens, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn independent_core_waits_for_external_terminal_and_persists_child_session() {
        let _guard = crate::test_support::storage_test_guard_async().await;
        let storage = tempfile::tempdir().unwrap();
        let server = OneShotSseServer::start();

        // 真实 Core 注册一个最小插件，取得合法的父反馈通道供子事件桥转发。
        // 投递一条消息触发 worker 循环（engine 构建 + 插件注册），不等 turn 完成。
        let capture = Arc::new(FeedbackCapturePlugin::default());
        let mut parent_session = Session::new("parent");
        parent_session.cwd = storage.path().to_string_lossy().into_owned();
        parent_session.bind_storage_root(storage.path());
        let (parent_events_tx, _parent_events_rx) = std::sync::mpsc::channel();
        let parent_core = TiangongCore::builder()
            .config(CoreConfigProvider::new(CoreConfig::default()))
            .session(parent_session)
            .event_sender(parent_events_tx)
            .plugins(vec![capture.clone()])
            .storage(CoreStorageLocation::new(storage.path()))
            .build()
            .unwrap();
        parent_core
            .deliver(AgentInputKind::message("trigger plugin registration"))
            .unwrap();
        let feedback = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(feedback) = capture.feedback.lock().unwrap().clone() {
                    break feedback;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("父 Core 应完成插件注册");

        let agent_id = "agent-child-runtime".to_string();
        let descriptor = AgentDescriptor {
            agent_id: agent_id.clone(),
            role: "dev".to_string(),
            label: "Developer".to_string(),
            system_prompt: "完成测试任务".to_string(),
            status: crate::state::AgentStatus::Idle,
        };
        let mut child_session = Session::new("Developer");
        child_session.id = agent_id.clone();
        child_session.cwd = storage.path().to_string_lossy().into_owned();
        child_session.parent_session_id = Some(parent_core.session_id().to_string());
        let child_storage = storage
            .path()
            .join("teams")
            .join(parent_core.session_id())
            .join(&agent_id);
        let mut config = CoreConfig::default();
        config.llm.chat = ModelEndpoint {
            base_url: server.base_url.clone(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            timeout_ms: 5_000,
            ..Default::default()
        };
        let base_feedback = Arc::new(RwLock::new(Some(feedback.clone())));

        // 旧的空闲终态不能抢先完成刚注册的 waiter；只有看到相同稳定消息 ID
        // 的 UserMessage 后，后续 Done 才属于这轮。
        let correlated_cache = Mutex::new(HashMap::new());
        let correlated_state = Mutex::new(RuntimeState::Running);
        let (correlated_tx, mut correlated_rx) = tokio::sync::oneshot::channel();
        let correlated_active = Mutex::new(Some(ActiveTurn {
            message_id: "delivery-correlated".to_string(),
            started: false,
            feedback: feedback.clone(),
            terminal_tx: correlated_tx,
            collector: TurnCollector::default(),
        }));
        handle_child_event(
            &descriptor,
            &correlated_active,
            &correlated_state,
            &correlated_cache,
            &base_feedback,
            StreamEvent::Done { usage: None },
        );
        assert!(matches!(
            correlated_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(*correlated_state.lock().unwrap(), RuntimeState::Running);
        handle_child_event(
            &descriptor,
            &correlated_active,
            &correlated_state,
            &correlated_cache,
            &base_feedback,
            StreamEvent::UserMessage {
                message_id: "delivery-correlated".to_string(),
                content: "test".to_string(),
                content_blocks: Vec::new(),
                media: Vec::new(),
                model_excluded: false,
            },
        );
        handle_child_event(
            &descriptor,
            &correlated_active,
            &correlated_state,
            &correlated_cache,
            &base_feedback,
            StreamEvent::Done {
                usage: Some(usage(3, 2)),
            },
        );
        let correlated = correlated_rx.await.unwrap().unwrap();
        assert_eq!(correlated.usage.total_tokens, 5);
        assert_eq!(*correlated_state.lock().unwrap(), RuntimeState::Idle);

        // 模拟 deliver Future 被取消：RunningGuard drop 时 waiter 仍存在，状态必须
        // 保持 Running；只有事件桥观察到终态后才能转为 Idle。
        let cancelled_cache = Mutex::new(HashMap::new());
        let cancelled_state = Mutex::new(RuntimeState::Idle);
        let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
        drop(cancelled_rx);
        let cancelled_active = Mutex::new(None);
        let cancelled_guard = RunningGuard::try_new(&cancelled_state, &cancelled_active).unwrap();
        *cancelled_active.lock().unwrap() = Some(ActiveTurn {
            message_id: "delivery-cancelled-waiter".to_string(),
            started: true,
            feedback: feedback.clone(),
            terminal_tx: cancelled_tx,
            collector: TurnCollector::default(),
        });
        drop(cancelled_guard);
        assert_eq!(*cancelled_state.lock().unwrap(), RuntimeState::Running);
        handle_child_event(
            &descriptor,
            &cancelled_active,
            &cancelled_state,
            &cancelled_cache,
            &base_feedback,
            StreamEvent::Done {
                usage: Some(usage(8, 1)),
            },
        );
        assert_eq!(*cancelled_state.lock().unwrap(), RuntimeState::Idle);
        assert_eq!(
            cancelled_cache
                .lock()
                .unwrap()
                .get("delivery-cancelled-waiter")
                .unwrap()
                .usage
                .total_tokens,
            0
        );

        let child = ChildRuntime::start(
            descriptor,
            child_session,
            config,
            child_storage.clone(),
            storage.path().to_path_buf(),
            Vec::new(),
            Arc::new(EmptyPlugin),
            base_feedback,
        )
        .unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            child.deliver_and_wait(
                "delivery-real-sse".to_string(),
                vec![ContentBlock::text("执行真实事件流测试")],
                feedback.clone(),
            ),
        )
        .await
        .expect("子 Core 应在 external Done 后完成")
        .unwrap();
        assert_eq!(result.status, TurnStatus::Success);
        assert_eq!(result.assistant_text.as_deref(), Some("子 Agent 已完成"));
        assert_eq!(result.usage.total_tokens, 15);

        let session_path = child_storage
            .join("sessions")
            .join(format!("{agent_id}.json"));
        assert!(session_path.is_file());
        let restored = Session::load_from_storage(&child_storage, &agent_id).unwrap();
        let persisted_turn = restored
            .messages
            .iter()
            .find(|message| message.id == "delivery-real-sse")
            .unwrap();
        assert_eq!(persisted_turn.turn_status, Some(TurnStatus::Success));

        // 只要 cancel 已观察到 Running，command_gate 就保证 Message 已先入队。
        // 若顺序反转，空闲 Cancel 会先被消费并清除，随后消息会卡在本测试的模型服务。
        let stalling_server = StallingServer::start();
        let mut stalling_config = CoreConfig::default();
        stalling_config.llm.chat = ModelEndpoint {
            base_url: stalling_server.base_url.clone(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            timeout_ms: 5_000,
            ..Default::default()
        };
        child.replace_base_config(&stalling_config).unwrap();
        let delivery_child = Arc::clone(&child);
        let active_feedback = feedback.clone();
        let delivery = tokio::spawn(async move {
            delivery_child
                .deliver_and_wait(
                    "delivery-cancel-order".to_string(),
                    vec![ContentBlock::text("等待取消")],
                    active_feedback,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !child.is_running() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("子 Core 应进入 Running");

        // 第二个等待者还卡在 delivery_gate，按它的 message_id 取消不能误伤当前轮次。
        let queued_child = Arc::clone(&child);
        let queued = tokio::spawn(async move {
            queued_child
                .deliver_and_wait(
                    "delivery-queued".to_string(),
                    vec![ContentBlock::text("排队等待")],
                    feedback,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!child.cancel_if_active("delivery-queued"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!delivery.is_finished(), "排队等待者不得取消当前轮次");
        queued.abort();
        assert!(queued.await.unwrap_err().is_cancelled());

        assert!(
            child.cancel_if_active("delivery-cancel-order"),
            "匹配当前消息的 Cancel 应成功排在 Message 之后"
        );
        let cancelled = tokio::time::timeout(Duration::from_secs(3), delivery)
            .await
            .expect("正确排序的 Cancel 应及时结束子轮次")
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.status, TurnStatus::Cancelled);

        tokio::time::timeout(Duration::from_secs(3), child.shutdown())
            .await
            .expect("shutdown 必须等待子 Core 和事件线程退出")
            .unwrap();
        tokio::task::spawn_blocking(move || parent_core.shutdown_join())
            .await
            .unwrap()
            .unwrap();
    }
}
