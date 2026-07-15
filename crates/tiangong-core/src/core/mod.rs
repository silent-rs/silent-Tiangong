//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, Sender as StdSender};
use std::sync::{Arc, Mutex};
use std::thread::{self};
use tokio::sync::mpsc as tokio_mpsc;

use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::model::{SingleProviderClient, ToolSpec};
use crate::react::message::accept_user_message;
use crate::session::{MessageRole, Session};
use tiangong_types::{ContentBlock, SessionStreamEvent, StreamEvent};

/// 单次工具执行阶段（ReAct Loop 内层）的最大轮次。
///
/// 30 轮：review 类多步骤任务（连续读取多个文件做审查）在 15 轮时容易触顶，
/// 过早进入总结阶段。30 轮给复杂任务更多空间；触顶后仍走与正常完成相同的总结
/// 路径（由模型自行判断完成度），无需特殊触顶逻辑。

/// 总结阶段后重新进入工具执行阶段的最大次数。
pub mod command;
pub(crate) use command::Command;
pub mod plugin;
pub use plugin::Plugin;
pub mod builder;
pub mod error;
pub mod storage_location;

pub use builder::TiangongCoreBuilder;
pub use error::CoreError;
pub use storage_location::CoreStorageLocation;

/// 天工智能体核心
pub struct TiangongCore {
    /// 会话 ID
    session_id: String,
    /// 当前 Core 独立的配置提供者。
    config: CoreConfigProvider,
    /// 会话信任模式。
    trust_mode: std::sync::Mutex<crate::permission::TrustMode>,
    /// 存储根目录(turn task 从此加载/保存 session)。
    storage_root: std::path::PathBuf,
    /// 事件输出通道(会话级,跨 turn;clone 给每个 turn task 的 forwarder)。
    external_tx: Sender<SessionStreamEvent>,
    /// 进程内插件(会话级,跨 turn)。
    plugins: Vec<Arc<dyn Plugin>>,
    /// on_session_ready 是否已触发(跨 turn)。
    session_ready_fired: Arc<std::sync::atomic::AtomicBool>,
}

impl TiangongCore {
    /// Builder 的实际装配实现（私有）。
    ///
    /// worker task 由共享 runtime 的 `spawn` 创建（非 OS 线程），构造期不会失败；
    /// `build()` 的 `Result` 仅承载必填字段缺失的检查。空闲时 worker task 停在
    /// `cmd_rx.recv().await`，future 被 park、线程归还 runtime 池。
    fn assemble(
        session_id: String,
        config: CoreConfigProvider,
        trust_mode: crate::permission::TrustMode,
        storage_root: std::path::PathBuf,
        external_tx: Sender<SessionStreamEvent>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        Self {
            session_id,
            config,
            trust_mode: std::sync::Mutex::new(trust_mode),
            storage_root,
            external_tx,
            plugins,
            session_ready_fired: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Builder 入口：与宿主入口（GUI/CLI/Server）解耦的构造方式。
    ///
    /// session 为必填字段，新会话由调用方创建后传入。
    pub fn builder() -> TiangongCoreBuilder {
        TiangongCoreBuilder::default()
    }

    /// 向活跃 turn task 发送命令(无活跃 task 则忽略)。
    fn send_cmd(&self, cmd: Command) -> Result<(), CoreError> {
        crate::shared_runtime::send_command(&self.session_id, cmd);
        Ok(())
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 替换该 Core 的独立配置。
    ///
    /// 配置在下一次 turn 构建 engine 时自动生效（每 turn 现建 engine 会读取最新
    /// generation），无需显式通知 worker。
    pub fn replace_config(&self, config: CoreConfig) -> Result<(), CoreError> {
        self.config.replace(config);
        Ok(())
    }

    /// 是否已停止（worker task 已结束或命令通道已关闭）。
    ///
    /// 已停止的 core 无法再接收命令，调用方应移除并按需重建。
    /// 是否有活跃的 turn task。
    pub fn is_stopped(&self) -> bool {
        !crate::shared_runtime::is_running(&self.session_id)
    }

    /// 是否正在执行 turn(有活跃 turn task)。
    pub fn is_busy(&self) -> bool {
        crate::shared_runtime::is_running(&self.session_id)
    }

    /// 设置会话信任模式。
    ///
    /// 更新后对当前会话的权限门与插件实时生效。
    pub fn set_trust_mode(&self, mode: crate::permission::TrustMode) {
        // 更新 Core 持有的值(下次 turn 用) + 发送到活跃 turn task(即时生效)
        *self.trust_mode.lock().unwrap() = mode;
        let _ = self.send_cmd(Command::SetTrustMode(mode));
    }

    /// 关闭并获取最终 session。
    ///
    /// worker panic 时返回 [`CoreError::WorkerPanicked`]，不再静默兜底为
    /// `Session::new("recovered")`——避免丢失原会话数据后调用方误判成功。
    /// 从磁盘加载最终 session。
    pub fn into_session(self) -> Result<Session, CoreError> {
        // 发 Cancel 终止活跃 turn(如有)
        let _ = self.send_cmd(Command::Cancel);
        Session::load_from_storage(&self.storage_root, &self.session_id)
            .map_err(|_| CoreError::WorkerPanicked)
    }

    /// 关闭 Core,不取回 session(session 已在磁盘上)。
    pub fn shutdown_join(self) -> Result<(), CoreError> {
        let _ = self.send_cmd(Command::Cancel);
        Ok(())
    }
}

impl crate::agent_input::AgentInput for TiangongCore {
    fn deliver(&self, input: crate::agent_input::AgentInputKind) -> Result<(), CoreError> {
        use crate::agent_input::{AgentInputKind, ApprovalInput, CommandInput, MessageInput};

        match input {
            AgentInputKind::Message(MessageInput::UserMessage {
                prepared,
                message_id,
            }) => {
                let session_id = self.session_id.clone();

                // 1. 从磁盘加载 session
                let mut session = Session::load_from_storage(&self.storage_root, &session_id)
                    .map_err(|e| CoreError::WorkerStopped)?;

                // 2. 内部 stream 通道(per-turn forwarder)
                let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();

                // 3. 将用户消息注入 session 并落盘
                let message_id = message_id.unwrap_or_else(|| scru128::new().to_string());
                let accepted =
                    accept_user_message(&mut session, &stream_tx, Some(message_id), prepared, true)
                        .map_err(|e| CoreError::WorkerStopped)?;
                let turn_start_idx = accepted.turn_start_idx;
                let user_msg_id = accepted.message_id.clone();
                let accepted_prepared = accepted.prepared.clone();

                // 4. 构建 TurnContext(session 在其中)
                let trust_mode = self
                    .trust_mode
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                let mut session_ready_flag = self
                    .session_ready_fired
                    .load(std::sync::atomic::Ordering::Relaxed);
                let (turn_cmd_tx, turn_cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();
                let mut session_ready_flag = self
                    .session_ready_fired
                    .load(std::sync::atomic::Ordering::Relaxed);
                let (mut ctx, _tools) = build_turn_context(
                    &self.config,
                    &stream_tx,
                    trust_mode,
                    &self.storage_root,
                    &self.plugins,
                    turn_cmd_tx.clone(),
                    &mut session,
                    &mut session_ready_flag,
                );
                self.session_ready_fired
                    .store(session_ready_flag, std::sync::atomic::Ordering::Relaxed);

                // session 现在在 ctx 内,TurnContext 持有它
                let external_tx = self.external_tx.clone();

                // 5. spawn turn task(传入 ctx + 执行参数)
                let (turn_cmd_tx, turn_cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();
                let sid = session_id.clone();
                crate::shared_runtime::spawn_turn(session_id, move |_ignored_rx| async move {
                    run_turn(
                        sid,
                        ctx,
                        stream_tx,
                        stream_rx,
                        external_tx,
                        turn_cmd_rx,
                        turn_start_idx,
                        user_msg_id,
                    )
                    .await;
                });
                // turn_cmd_tx 被 drop 后通道关闭;Cancel 等命令通过 send_command 投递
                std::mem::forget(turn_cmd_tx); // TODO: turn_cmd_tx 应该存到 TURN_TASKS
                Ok(())
            }
            AgentInputKind::Tool(tool) => self.send_cmd(Command::InjectTool {
                tool_name: tool.tool_name().to_string(),
                payload: tool.render(),
            }),
            AgentInputKind::Approval(ApprovalInput::Response {
                request_id,
                approved,
            }) => self.send_cmd(Command::Approval {
                request_id,
                approved,
            }),
            AgentInputKind::Command(cmd) => match cmd {
                CommandInput::Cancel => self.send_cmd(Command::Cancel),
                CommandInput::CompressContext => self.send_cmd(Command::CompressContext),
                CommandInput::ResetContext => self.send_cmd(Command::ResetContext),
            },
        }
    }
}

impl Drop for TiangongCore {
    fn drop(&mut self) {
        // 发 Cancel 终止活跃 turn(如有)
        crate::shared_runtime::send_command(&self.session_id, Command::Cancel);
    }
}

/// turn task:执行 TurnContext 中的 turn。
///
/// TurnContext(session + 用户消息已注入)在 deliver 中构建,传入此处执行。
/// turn 结束后 session 落盘,task 退出。
#[allow(clippy::too_many_arguments)]
async fn run_turn(
    session_id: String,
    mut ctx: crate::turn_context::TurnContext,
    stream_tx: StdSender<StreamEvent>,
    stream_rx: std::sync::mpsc::Receiver<StreamEvent>,
    external_tx: Sender<SessionStreamEvent>,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
    turn_start_idx: usize,
    user_msg_id: String,
) {
    // ===== per-turn forwarder + 屏障 =====
    let fwd_session_id = session_id.clone();
    let fwd_tx = external_tx.clone();
    let turn_capture = Arc::new(TurnCaptureState {
        capture: Mutex::new(TurnCapture::default()),
        notify: tokio::sync::Notify::new(),
    });
    let fwd_capture = turn_capture.clone();
    let forward_terminal = Arc::new(AtomicBool::new(false));
    let fwd_terminal = forward_terminal.clone();
    let mut turn_boundary_id = 0u64;

    let forward_handle = thread::spawn(move || {
        let mut external_open = true;
        while let Ok(event) = stream_rx.recv() {
            if let StreamEvent::TokenUsage {
                usage,
                agent_id,
                source,
                ..
            } = &event
            {
                if let Ok(mut capture) = fwd_capture.capture.lock() {
                    capture.usage.record(usage, agent_id.as_deref(), source);
                }
            }
            match event {
                StreamEvent::TurnBoundary { boundary_id } => {
                    if let Ok(mut capture) = fwd_capture.capture.lock() {
                        capture.processed_boundary = capture.processed_boundary.max(boundary_id);
                    }
                    fwd_capture.notify.notify_waiters();
                    continue;
                }
                terminal @ (StreamEvent::Done { .. } | StreamEvent::Error { .. }) => {
                    if !fwd_terminal.swap(false, Ordering::AcqRel) {
                        if let Ok(mut capture) = fwd_capture.capture.lock() {
                            capture.terminal = Some(terminal);
                        }
                        continue;
                    }
                    if external_open
                        && fwd_tx
                            .send(SessionStreamEvent {
                                session_id: fwd_session_id.clone(),
                                event: terminal,
                            })
                            .is_err()
                    {
                        external_open = false;
                    }
                    continue;
                }
                _ => {}
            }
            if external_open
                && fwd_tx
                    .send(SessionStreamEvent {
                        session_id: fwd_session_id.clone(),
                        event,
                    })
                    .is_err()
            {
                external_open = false;
            }
        }
    });

    // ===== execute turn =====
    let turn_started = std::time::Instant::now();
    let turn_start_cwd = ctx.session.cwd.clone();
    forward_terminal.store(false, Ordering::Release);
    if let Ok(mut capture) = turn_capture.capture.lock() {
        capture.terminal = None;
        capture.usage = TurnUsageCapture::default();
    }

    // on_turn_started(session 在 ctx 内)
    let plugins = ctx.tools.clone(); // TODO: plugins 需要单独传入或从 ctx 获取
    // 插件钩子需要 &mut session — 从 ctx 借用
    let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));

    execute_turn_async(
        &mut session,
        &user_msg_id,
        &[],
        &ctx,
        &stream_tx,
        &mut cmd_rx,
        &session_id,
    )
    .await;

    // ===== 等终态 =====
    turn_boundary_id = turn_boundary_id.wrapping_add(1);
    let boundary_id = turn_boundary_id;
    let _ = stream_tx.send(StreamEvent::TurnBoundary { boundary_id });
    let mut terminal = loop {
        {
            let mut capture = turn_capture
                .capture
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if capture.processed_boundary >= boundary_id {
                break capture.terminal.take();
            }
        }
        turn_capture.notify.notified().await;
    }
    .unwrap_or(StreamEvent::Done { usage: None });

    if session.cwd != turn_start_cwd {
        let workspace_path = std::path::PathBuf::from(&session.cwd);
        let workspace = workspace_path.is_dir().then_some(workspace_path.as_path());
        // TODO: plugin.set_workspace
    }

    let interrupted_tools =
        crate::react::message::close_unfinished_tool_calls_for_turn(&mut session);
    if !interrupted_tools.is_empty() {
        for (tool_call_id, tool_name, output) in interrupted_tools {
            let _ = stream_tx.send(StreamEvent::ToolResult {
                name: tool_name,
                tool_call_id: Some(tool_call_id),
                ok: false,
                output,
                full_output: None,
                duration_ms: None,
            });
        }
        if matches!(terminal, StreamEvent::Done { .. }) {
            terminal = StreamEvent::Error {
                message: "本轮仍有未完成的工具调用，已安全中断".to_string(),
            };
        }
    }

    crate::react::message::flush_deferred_tool_injections(&mut session, &stream_tx);

    // on_turn_finished — TODO: 需要 plugins 引用

    wait_for_stream_boundary(&stream_tx, &turn_capture, &mut turn_boundary_id).await;
    let turn_usage = {
        let mut capture = turn_capture
            .capture
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::mem::take(&mut capture.usage)
    };
    turn_usage.apply_to_session(&mut session);
    session.clear_transient_content();

    let elapsed_ms = turn_started.elapsed().as_millis() as u64;
    let mut status = TurnOutcome::from_terminal(&terminal).status;

    if status == tiangong_types::TurnStatus::Cancelled {
        // TODO: plugin.on_cancel
    }

    if let Some(msg) = session
        .messages
        .iter_mut()
        .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
    {
        msg.set_turn_result(elapsed_ms, status);
    }
    let _ = session.try_persist_to_disk();

    send_final_stream_event(
        &stream_tx,
        &forward_terminal,
        &turn_capture,
        &mut turn_boundary_id,
        terminal,
    )
    .await;

    // session 放回 ctx(drop 时不需要额外操作)
    ctx.session = session;

    // ===== 清理 forwarder =====
    drop(stream_tx);
    let _ = tokio::task::spawn_blocking(move || forward_handle.join()).await;
}

fn process_shutdown_feedback_command(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    command: Command,
    usage_capture: &mut TurnUsageCapture,
    trust_mode: crate::permission::TrustMode,
) {
    let _ = trust_mode;
    match command {
        Command::InjectTool { tool_name, payload } => {
            crate::react::message::defer_tool_injection(session, stream_tx, tool_name, payload);
            crate::react::message::flush_deferred_tool_injections(session, stream_tx);
        }
        Command::EmitStreamEvent(event)
            if !matches!(*event, StreamEvent::Done { .. } | StreamEvent::Error { .. }) =>
        {
            let event = *event;
            if let StreamEvent::TokenUsage {
                usage,
                agent_id,
                source,
                ..
            } = &event
            {
                let delta = usage_capture.record(usage, agent_id.as_deref(), source);
                apply_usage_delta_to_session(session, &delta, agent_id.as_deref());
                session.updated_at = crate::session::now_text();
            }
            let _ = stream_tx.send(event);
        }
        Command::EmitStreamEvent(_) => {}
        _ => {}
    }
}

pub(crate) fn apply_session_cwd(session: &Session) {
    let cwd = session.cwd.trim();
    if cwd.is_empty() {
        tiangong_toolkit::set_session_cwd(None);
        return;
    }

    let path = std::path::PathBuf::from(cwd);
    if path.is_dir() {
        tiangong_toolkit::set_session_cwd(Some(path));
    }
}

pub(crate) async fn compress_context_for_session(
    session: &mut Session,
    ctx: &crate::turn_context::TurnContext,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) {
    let _ = stream_tx.send(StreamEvent::ContextCompressing {
        summary_up_to: session.summary_up_to,
        total_messages: session.messages.len(),
    });
    let organizer = crate::context::organizer::ContextOrganizer::new(ctx.context_limit)
        .with_keep_recent_turns(6);
    let update = tokio::select! {
        update = organizer.force_update_summary_with_usage_async(session, ctx.client()) => update,
        cmd = cmd_rx.recv() => {
            let _ = cmd;
            let _ = stream_tx.send(StreamEvent::AgentNotification {
                agent_id: "system".to_string(),
                agent_label: "系统".to_string(),
                content: "上下文压缩已取消".to_string(),
                level: "warning".to_string(),
            });
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: tiangong_types::stream::ContextCompressAction::Cancelled,
                summary_up_to: session.summary_up_to,
                remaining_messages: session.messages.len().saturating_sub(session.summary_up_to),
            });
            return;
        }
    };
    match update {
        Ok(update) => {
            let remaining = session.messages.len().saturating_sub(session.summary_up_to);
            session.current_tokens = 0;
            session.active_agent_current_tokens = 0;
            session.agent_current_tokens.clear();
            crate::react::context::rebuild_system_prompt(session, ctx);
            crate::react::context::emit_token_usage(
                stream_tx,
                &update.usage,
                Some(0),
                ctx.context_limit,
                "manual_context_compress",
                None,
            );
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: if update.compressed {
                    tiangong_types::stream::ContextCompressAction::Compress
                } else {
                    tiangong_types::stream::ContextCompressAction::Noop
                },
                summary_up_to: session.summary_up_to,
                remaining_messages: remaining,
            });
            session.persist_to_disk();
        }
        Err(err) => {
            let _ = stream_tx.send(StreamEvent::AgentNotification {
                agent_id: "system".to_string(),
                agent_label: "系统".to_string(),
                content: format!("上下文压缩失败：{err}"),
                level: "error".to_string(),
            });
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: tiangong_types::stream::ContextCompressAction::Failed,
                summary_up_to: session.summary_up_to,
                remaining_messages: session.messages.len().saturating_sub(session.summary_up_to),
            });
        }
    }
}

pub(crate) fn reset_context_for_session(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    ctx: &crate::turn_context::TurnContext,
) {
    let total = session.messages.len();
    session.summary_up_to = total;
    crate::context::compressor::mark_compact_boundary(&mut session.messages, total);
    session.context_summary = None;
    session.current_tokens = 0;
    session.active_agent_current_tokens = 0;
    session.agent_current_tokens.clear();
    // 清空后重建 system prompt
    crate::react::context::rebuild_system_prompt(session, ctx);
    let _ = stream_tx.send(StreamEvent::ContextCompressed {
        action: tiangong_types::stream::ContextCompressAction::Clear,
        summary_up_to: total,
        remaining_messages: 0,
    });
    session.persist_to_disk();
}

/// 转发线程与 worker 之间的轮次终态/屏障状态。
#[derive(Default)]
struct TurnCapture {
    terminal: Option<StreamEvent>,
    usage: TurnUsageCapture,
    processed_boundary: u64,
}

/// 转发线程与 worker 共享的屏障状态。
///
/// `capture` 用 std::sync::Mutex 保护（双方都短时持锁，不跨 await）；
/// `notify` 让 worker（async task）在不使用 block_in_place 的情况下等待
/// 转发线程处理完 TurnBoundary。
struct TurnCaptureState {
    capture: Mutex<TurnCapture>,
    notify: tokio::sync::Notify,
}

/// 转发线程捕获的单轮精确用量。
///
/// `total` 同时包含主 Agent 与所有子 Agent；`by_agent` 额外保留子 Agent 维度，
/// 供会话切换到 Agent Tab 时恢复各自累计值。
#[derive(Default, Clone)]
struct TurnUsageCapture {
    total: tiangong_types::TokenUsage,
    by_agent: HashMap<String, tiangong_types::TokenUsage>,
    /// 每个执行单元已经观测到的用量，用于把显式标记为
    /// `source=cancelled-cumulative` 的累计快照换算成尚未落账的增量。
    /// `None` 表示主执行单元。普通 `cancelled-incremental` 事件始终按增量记账。
    observed_by_actor: HashMap<Option<String>, tiangong_types::TokenUsage>,
}

impl TurnUsageCapture {
    fn record(
        &mut self,
        usage: &tiangong_types::TokenUsage,
        agent_id: Option<&str>,
        source: &str,
    ) -> tiangong_types::TokenUsage {
        let mut normalized = usage.clone();
        if normalized.total_tokens == 0 {
            normalized.total_tokens = normalized.prompt_tokens + normalized.completion_tokens;
        }
        if normalized.total_tokens == 0 {
            return tiangong_types::TokenUsage::default();
        }

        let actor = agent_id.map(str::to_string);
        let observed = self.observed_by_actor.entry(actor).or_default();
        let delta = if source == "cancelled-cumulative" {
            let delta = cumulative_usage_delta(&normalized, observed);
            merge_cumulative_usage(observed, &normalized);
            delta
        } else {
            observed.accumulate(&normalized);
            normalized
        };

        self.total.accumulate(&delta);
        if let Some(agent_id) = agent_id {
            self.by_agent
                .entry(agent_id.to_string())
                .or_default()
                .accumulate(&delta);
        }
        delta
    }

    fn apply_to_session(self, session: &mut Session) {
        if self.total.total_tokens > 0 {
            session.token_usage.accumulate(&self.total);
        }
        for (agent_id, usage) in self.by_agent {
            session
                .agent_token_usage
                .entry(agent_id)
                .or_default()
                .accumulate(&usage);
        }
    }
}

fn cumulative_usage_delta(
    cumulative: &tiangong_types::TokenUsage,
    observed: &tiangong_types::TokenUsage,
) -> tiangong_types::TokenUsage {
    tiangong_types::TokenUsage {
        prompt_tokens: cumulative
            .prompt_tokens
            .saturating_sub(observed.prompt_tokens),
        completion_tokens: cumulative
            .completion_tokens
            .saturating_sub(observed.completion_tokens),
        total_tokens: cumulative
            .total_tokens
            .saturating_sub(observed.total_tokens),
        prompt_cache_hit_tokens: optional_usage_delta(
            cumulative.prompt_cache_hit_tokens,
            observed.prompt_cache_hit_tokens,
        ),
        prompt_cache_miss_tokens: optional_usage_delta(
            cumulative.prompt_cache_miss_tokens,
            observed.prompt_cache_miss_tokens,
        ),
    }
}

fn optional_usage_delta(cumulative: Option<usize>, observed: Option<usize>) -> Option<usize> {
    cumulative.map(|value| value.saturating_sub(observed.unwrap_or_default()))
}

fn merge_cumulative_usage(
    observed: &mut tiangong_types::TokenUsage,
    cumulative: &tiangong_types::TokenUsage,
) {
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

fn apply_usage_delta_to_session(
    session: &mut Session,
    usage: &tiangong_types::TokenUsage,
    agent_id: Option<&str>,
) {
    if usage.total_tokens == 0 {
        return;
    }
    session.token_usage.accumulate(usage);
    if let Some(agent_id) = agent_id {
        session
            .agent_token_usage
            .entry(agent_id.to_string())
            .or_default()
            .accumulate(usage);
    }
}

#[cfg(test)]
mod turn_usage_capture_tests {
    use super::*;

    fn usage(
        prompt_tokens: usize,
        completion_tokens: usize,
        total_tokens: usize,
    ) -> tiangong_types::TokenUsage {
        tiangong_types::TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }
    }

    #[test]
    fn normalizes_and_persists_main_and_agent_usage_once() {
        let mut capture = TurnUsageCapture::default();
        capture.record(&usage(10, 5, 0), None, "react-round-1");
        capture.record(&usage(20, 8, 28), Some("researcher"), "react-round-1");
        capture.record(&usage(7, 3, 10), Some("researcher"), "react-round-2");

        let mut session = Session::new("usage-capture");
        session.token_usage = usage(1, 1, 2);
        session
            .agent_token_usage
            .insert("researcher".to_string(), usage(2, 1, 3));
        capture.apply_to_session(&mut session);

        assert_eq!(session.token_usage.prompt_tokens, 38);
        assert_eq!(session.token_usage.completion_tokens, 17);
        assert_eq!(session.token_usage.total_tokens, 55);

        let agent_usage = session.agent_token_usage.get("researcher").unwrap();
        assert_eq!(agent_usage.prompt_tokens, 29);
        assert_eq!(agent_usage.completion_tokens, 12);
        assert_eq!(agent_usage.total_tokens, 41);
    }

    #[test]
    fn cancelled_cumulative_usage_only_records_unseen_delta() {
        let mut capture = TurnUsageCapture::default();
        capture.record(&usage(10, 5, 15), None, "react-round-1");
        let delta = capture.record(&usage(14, 8, 22), None, "cancelled-cumulative");

        assert_eq!(delta.prompt_tokens, 4);
        assert_eq!(delta.completion_tokens, 3);
        assert_eq!(delta.total_tokens, 7);

        let mut session = Session::new("cancelled-usage-capture");
        capture.apply_to_session(&mut session);
        assert_eq!(session.token_usage.prompt_tokens, 14);
        assert_eq!(session.token_usage.completion_tokens, 8);
        assert_eq!(session.token_usage.total_tokens, 22);
    }

    #[test]
    fn shutdown_background_usage_delta_can_be_applied_immediately() {
        let mut capture = TurnUsageCapture::default();
        let first = capture.record(&usage(20, 10, 30), Some("child-agent"), "react-round-1");
        let cancelled = capture.record(
            &usage(25, 12, 37),
            Some("child-agent"),
            "cancelled-cumulative",
        );
        let mut session = Session::new("shutdown-usage-capture");

        apply_usage_delta_to_session(&mut session, &first, Some("child-agent"));
        apply_usage_delta_to_session(&mut session, &cancelled, Some("child-agent"));

        assert_eq!(session.token_usage.total_tokens, 37);
        assert_eq!(
            session
                .agent_token_usage
                .get("child-agent")
                .unwrap()
                .total_tokens,
            37
        );
    }

    #[test]
    fn summary_cancelled_increment_is_added_after_react_usage() {
        let mut capture = TurnUsageCapture::default();
        capture.record(&usage(70, 30, 100), None, "react-round-1");
        capture.record(&usage(15, 5, 20), None, "cancelled-incremental");

        let mut session = Session::new("summary-cancelled-increment");
        capture.apply_to_session(&mut session);

        assert_eq!(session.token_usage.prompt_tokens, 85);
        assert_eq!(session.token_usage.completion_tokens, 35);
        assert_eq!(session.token_usage.total_tokens, 120);
    }

    #[test]
    fn shutdown_feedback_persists_incremental_and_cancelled_usage_once() {
        let mut session = Session::new("shutdown-feedback-usage");
        let mut capture = TurnUsageCapture::default();
        let (stream_tx, stream_rx) = std::sync::mpsc::channel();
        let trust_mode = session.trust_mode;

        for (usage, source) in [
            (usage(20, 10, 30), "react-round-1"),
            (usage(25, 12, 37), "cancelled-cumulative"),
        ] {
            process_shutdown_feedback_command(
                &mut session,
                &stream_tx,
                Command::EmitStreamEvent(Box::new(StreamEvent::TokenUsage {
                    usage,
                    current_tokens: None,
                    compression_threshold_tokens: None,
                    context_limit_tokens: None,
                    source: source.to_string(),
                    agent_id: Some("child-agent".to_string()),
                })),
                &mut capture,
                trust_mode,
            );
        }

        assert_eq!(session.token_usage.total_tokens, 37);
        assert_eq!(
            session
                .agent_token_usage
                .get("child-agent")
                .unwrap()
                .total_tokens,
            37
        );
        assert_eq!(stream_rx.try_iter().count(), 2);
    }
}

#[cfg(test)]
mod shared_runtime_tests {
    use super::*;
    use crate::core::storage_location::CoreStorageLocation;
    use crate::core_config::{CoreConfig, CoreConfigProvider};

    /// 多个 Core 共享同一个 runtime，空闲时各自停在 cmd_rx.recv().await。
    /// 验证构造期不 panic、Core 可正常 deliver + shutdown。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiple_cores_share_runtime_and_shutdown_cleanly() {
        let root = tempfile::tempdir().unwrap();

        let config = CoreConfigProvider::new(CoreConfig::default());
        let cores: Vec<TiangongCore> = (0..3)
            .map(|i| {
                let session = Session::new(&format!("shared-runtime-{i}"));
                let (event_tx, _event_rx) = std::sync::mpsc::channel();
                TiangongCore::builder()
                    .config(config.clone())
                    .session(session)
                    .event_sender(event_tx)
                    .storage(CoreStorageLocation::new(root.path()))
                    .build()
                    .unwrap()
            })
            .collect();

        // 所有 Core 初始即存活（worker task 已 spawn 但 idle 在 recv().await）
        for core in &cores {
            assert!(!core.is_stopped(), "Core 应存活");
        }

        // 逐个关闭，验证 shutdown 路径在共享 runtime 上正常工作
        for core in cores {
            let result = tokio::task::spawn_blocking(move || core.shutdown_join()).await;
            assert!(result.is_ok(), "shutdown_join 不应 panic");
        }
    }
}

async fn send_final_stream_event(
    stream_tx: &StdSender<StreamEvent>,
    forward_terminal: &AtomicBool,
    turn_capture: &Arc<TurnCaptureState>,
    turn_boundary_id: &mut u64,
    event: StreamEvent,
) {
    debug_assert!(matches!(
        event,
        StreamEvent::Done { .. } | StreamEvent::Error { .. }
    ));
    forward_terminal.store(true, Ordering::Release);
    if stream_tx.send(event).is_ok() {
        // 必须确认转发线程已经处理完这个终态，才能接收/重置下一轮。否则新轮次
        // 清除放行标志时，上一轮尚在队列里的终态可能被误当作内部事件吞掉。
        wait_for_stream_boundary(stream_tx, turn_capture, turn_boundary_id).await;
    }
    forward_terminal.store(false, Ordering::Release);
}

async fn wait_for_stream_boundary(
    stream_tx: &StdSender<StreamEvent>,
    turn_capture: &Arc<TurnCaptureState>,
    turn_boundary_id: &mut u64,
) {
    *turn_boundary_id = turn_boundary_id.wrapping_add(1);
    let boundary_id = *turn_boundary_id;
    if stream_tx
        .send(StreamEvent::TurnBoundary { boundary_id })
        .is_err()
    {
        return;
    }
    loop {
        {
            let capture = turn_capture
                .capture
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if capture.processed_boundary >= boundary_id {
                return;
            }
        }
        turn_capture.notify.notified().await;
    }
}

/// 转发线程捕获的单个对话轮次终态。
///
/// `status` 由终态事件推导：`Done` → Success；`Error` 文案含「取消/cancel/abort」
/// 时为 Cancelled，否则为 Failed。worker_loop_async 据此把 `status` 与执行时长
/// 写入用户消息，供前端（含历史会话）展示。
struct TurnOutcome {
    status: tiangong_types::TurnStatus,
}

impl TurnOutcome {
    fn from_terminal(event: &StreamEvent) -> Self {
        let status = match event {
            StreamEvent::Done { .. } => tiangong_types::TurnStatus::Success,
            StreamEvent::Error { message } => {
                let lower = message.to_lowercase();
                if lower.contains("取消")
                    || lower.contains("cancel")
                    || lower.contains("abort")
                    || lower.contains("中断")
                {
                    tiangong_types::TurnStatus::Cancelled
                } else {
                    tiangong_types::TurnStatus::Failed
                }
            }
            _ => tiangong_types::TurnStatus::Failed,
        };
        Self { status }
    }
}

/// 执行一个完整的对话轮次（可能多轮工具调用），async 版
async fn execute_turn_async(
    session: &mut Session,
    message_id: &str,
    prepared: &[ContentBlock],
    ctx: &crate::turn_context::TurnContext,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    session_id: &str,
) {
    let mut turn_ctx = ctx.clone();
    turn_ctx.agent_id = session_id.to_string();
    turn_ctx
        .execute_turn(session, Some((message_id, prepared)), stream_tx, cmd_rx)
        .await;
}

/// 每 turn 现建 TurnContext：从最新 config 快照构建 client/权限/用量，
/// 共享会话级插件注册表，注入 turn 级 feedback，收集 tool_specs。
///
/// - 插件注册表（tool_overrides / prompt_section_providers）是会话级共享的
///   Arc<Mutex>，只在首次 turn 注册，后续 turn 通过 Arc clone 复用。
/// - 每 turn 必须刷新的是：feedback_tx（绑定新的 TurnUsageSink）、exec_env、
///   permission_overrides、tool_specs（可能因配置变更而不同）。
#[allow(clippy::too_many_arguments)]
fn build_turn_context(
    config: &CoreConfigProvider,
    stream_tx: &StdSender<StreamEvent>,
    mut trust_mode: crate::permission::TrustMode,
    storage_root: &std::path::Path,
    plugins: &[Arc<dyn Plugin>],
    cmd_tx: tokio_mpsc::UnboundedSender<Command>,
    session: &mut Session,
    session_ready_fired: &mut bool,
) -> (crate::turn_context::TurnContext, Vec<ToolSpec>) {
    let cfg = config.snapshot();
    let mut ctx =
        build_context_from_config(&cfg, stream_tx, trust_mode, storage_root.to_path_buf());

    let workspace = std::path::Path::new(&session.cwd);
    let workspace = workspace.is_dir().then_some(workspace);

    // 配置快照更新通知：在收集 specs 前调 on_config_updated，使插件先刷新
    // 内部 endpoint/client，再收集 tool_specs——保证 specs 基于最新配置。
    for plugin in plugins {
        plugin.on_config_updated(&cfg);
    }

    let is_first_turn = !*session_ready_fired;

    // 收集 tool_specs + 注入 turn 级状态（workspace/trust_mode/feedback_tx）。
    // 首次 turn 额外注册 provider/override/prompt 到共享注册表。
    let mut plugin_specs: Vec<ToolSpec> = Vec::new();
    let mut seen_tool_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for plugin in plugins {
        // 注入 turn 级状态（每 turn 都需要：feedback 绑定新 TurnUsageSink）
        plugin.set_workspace(workspace);
        plugin.set_trust_mode(trust_mode);
        plugin.set_feedback_tx(crate::core::plugin::PluginFeedbackTx::new(
            cmd_tx.clone(),
            ctx.turn_usage_sink().clone(),
        ));

        let specs = plugin.tool_specs();
        // 首次 turn：注册到共享注册表（后续 turn 通过 Arc 复用，不重复注册）
        if is_first_turn {
            let plugin_as_handler: Arc<dyn crate::tool_override::ToolOverrideHandler> =
                plugin.clone();
            for spec in &specs {
                ctx.register_tool_override(&spec.name, plugin_as_handler.clone());
            }
            let plugin_as_prompt: Arc<dyn crate::tool_override::PromptSectionProvider> =
                plugin.clone();
            ctx.register_prompt_section_provider(plugin_as_prompt);
        }

        for spec in specs {
            if seen_tool_names.insert(spec.name.clone()) {
                plugin_specs.push(spec);
            } else {
                tracing::debug!(
                    tool = %spec.name,
                    plugin = %plugin.id(),
                    "跳过与其他插件重名的工具规格（保留先注册者）"
                );
            }
        }
    }

    // 汇总 exec_env 注入给插件（配置可能变更，每 turn 刷新）
    {
        let mut exec_env = std::collections::BTreeMap::new();
        for plugin in plugins {
            for (key, value) in plugin.exec_env() {
                exec_env.insert(key, value);
            }
        }
        for plugin in plugins {
            plugin.set_exec_env(exec_env.clone());
        }
    }

    let injection_spec = crate::core::plugin::injection_tool_spec();
    let mut tools: Vec<ToolSpec> = Vec::new();
    tools.push(injection_spec);
    tools.extend(plugin_specs);
    ctx.tools = tools.clone();

    // 首次 context 构建 + 插件注册完成后触发一次 on_session_ready
    //（此时 workspace / trust_mode / feedback 已注入）；后续 turn 的再配置
    // 由各插件在 on_config_updated 中统一承载。
    if !*session_ready_fired {
        *session_ready_fired = true;
        for plugin in plugins {
            plugin.on_session_ready(session);
        }
    }

    (ctx, tools)
}

/// 从 CoreConfig 快照构建 TurnContext（每 turn 现建）
///
/// `stream_tx` 用于在 LLM 请求重试时发送 `StreamEvent::Retry` 通知。
/// `trust_mode` 是 TiangongCore 持有的会话信任解析句柄，TurnContext 共享它。
fn build_context_from_config(
    config: &crate::core_config::CoreConfig,
    stream_tx: &StdSender<StreamEvent>,
    mut trust_mode: crate::permission::TrustMode,
    storage_root: std::path::PathBuf,
) -> crate::turn_context::TurnContext {
    use crate::agent_config::AgentConfig;
    use crate::model::OnRetryCallback;
    use crate::turn_context::TurnContext;

    let agent_config = AgentConfig {
        trust_mode: config.trust_mode,
        default_trust_mode: config.default_trust_mode,
        custom_system_prompt: config.custom_system_prompt.clone(),
        reasoning_effort: config.reasoning_effort.clone(),
    };

    // 构造重试回调：发送 StreamEvent::Retry 通知前端
    let retry_tx = stream_tx.clone();
    let on_retry: OnRetryCallback =
        Arc::new(move |attempt, max_attempts, _delay_ms, error_text| {
            let _ = retry_tx.send(StreamEvent::Retry {
                message: error_text.to_string(),
                attempt,
                max_attempts,
            });
        });

    // context_limit 由 to_core_config 在加载时解析注入（core 不做配置磁盘 IO）。
    let context_limit = config.context_limit;
    let ctx = TurnContext::new(
        Session::new("placeholder"),
        SingleProviderClient::new(config.llm.chat.clone()).with_on_retry(on_retry.clone()),
        context_limit,
        agent_config,
        trust_mode,
        crate::observe::Observer::new(storage_root),
        Vec::new(),
        crate::MAX_TOOL_ROUNDS,
        crate::MAX_OUTER_ITERATIONS,
        Arc::new(crate::core::plugin::TurnUsageSink::new()),
    );
    ctx
}
