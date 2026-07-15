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
use crate::model::SingleProviderClient;
use crate::react::message::{AcceptedUserMessage, accept_user_message};
use crate::session::{MessageRole, Session};
use tiangong_types::{ContentBlock, StreamEvent};

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
    stream_tx: Sender<StreamEvent>,
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
        stream_tx: Sender<StreamEvent>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        Self {
            session_id,
            config,
            trust_mode: std::sync::Mutex::new(trust_mode),
            storage_root,
            stream_tx,
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
        crate::shared_runtime::send_command(&self.session_id, cmd)
            .then_some(())
            .ok_or(CoreError::WorkerStopped)
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
                // 如果有活跃 turn task,先取消并等待结束
                if crate::shared_runtime::is_running(&self.session_id) {
                    crate::shared_runtime::send_command(&self.session_id, Command::Cancel);
                    // 等待 task 结束(轮询,最多 5 秒)
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    while crate::shared_runtime::is_running(&self.session_id) {
                        if std::time::Instant::now() > deadline {
                            tracing::warn!(session_id = %self.session_id, "等待上一轮 turn 结束超时");
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }

                let session_id = self.session_id.clone();
                let session = Session::load_from_storage(&self.storage_root, &session_id).map_err(
                    |error| {
                        tracing::warn!(%error, %session_id, "加载本轮 Session 失败");
                        CoreError::WorkerStopped
                    },
                )?;
                let trust_mode = *self.trust_mode.lock().unwrap_or_else(|p| p.into_inner());
                let config = (*self.config.snapshot()).clone();
                let storage_root = self.storage_root.clone();
                let plugins = self.plugins.clone();
                let session_ready_fired = self.session_ready_fired.clone();
                // stream_tx 直接用 core 的通道(发 StreamEvent 到 app 层)
                let stream_tx = self.stream_tx.clone();

                let agent_config = crate::agent_config::AgentConfig {
                    trust_mode: config.trust_mode,
                    default_trust_mode: config.default_trust_mode,
                    custom_system_prompt: config.custom_system_prompt.clone(),
                    reasoning_effort: config.reasoning_effort.clone(),
                };
                let retry_tx = stream_tx.clone();
                let on_retry: crate::model::OnRetryCallback =
                    Arc::new(move |attempt, max_attempts, _delay_ms, error_text| {
                        let _ = retry_tx.send(StreamEvent::Retry {
                            message: error_text.to_string(),
                            attempt,
                            max_attempts,
                        });
                    });
                let client =
                    SingleProviderClient::new(config.llm.chat.clone()).with_on_retry(on_retry);
                let usage_sink = Arc::new(crate::core::plugin::TurnUsageSink::new());
                let prepared_plugins =
                    crate::core::plugin::prepare_plugins(&plugins, &config, trust_mode, &session);
                let mut ctx = crate::turn_context::TurnContext::builder()
                    .client(client)
                    .session(session)
                    .stream_tx(stream_tx)
                    .plugins(plugins)
                    .context_limit(config.context_limit)
                    .agent_config(agent_config)
                    .trust_mode(trust_mode)
                    .observer(crate::observe::Observer::new(storage_root))
                    .tool_overrides(prepared_plugins.tool_overrides)
                    .turn_usage_sink(usage_sink)
                    .tools(prepared_plugins.tools)
                    .build();

                let message_id = message_id.unwrap_or_else(|| scru128::new().to_string());
                let turn_stream_tx = ctx.stream_tx.clone();
                let accepted = accept_user_message(
                    &mut ctx.session,
                    &turn_stream_tx,
                    Some(message_id),
                    prepared,
                    true,
                )
                .map_err(|error| {
                    tracing::warn!(%error, "持久化本轮用户消息失败");
                    CoreError::WorkerStopped
                })?;

                crate::shared_runtime::spawn_turn(ctx, move |mut ctx, cmd_rx| {
                    if !session_ready_fired.swap(true, Ordering::AcqRel) {
                        for plugin in &ctx.plugins {
                            plugin.on_session_ready(&mut ctx.session);
                        }
                    }
                    let prompt_sections = ctx
                        .plugins
                        .iter()
                        .flat_map(|plugin| plugin.prompt_sections())
                        .collect();
                    ctx.session.rebuild_system_prompt(
                        &crate::prompt::SystemPromptConfig::from_plugin_sections(prompt_sections),
                    );
                    ctx.session.try_persist_to_disk().map_err(|error| {
                        tracing::warn!(%error, "持久化本轮系统提示失败");
                        CoreError::WorkerStopped
                    })?;
                    Ok(run_turn(ctx, cmd_rx, stream_tx, accepted))
                })
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
async fn run_turn(
    mut ctx: crate::turn_context::TurnContext,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
    stream_tx: Sender<StreamEvent>,
    accepted: AcceptedUserMessage,
) {
    let stream_tx = ctx.stream_tx.clone();
    let session_id = ctx.session.id.clone();

    let turn_capture = Arc::new(TurnCaptureState {
        capture: Mutex::new(TurnCapture::default()),
        notify: tokio::sync::Notify::new(),
    });
    let forward_terminal = Arc::new(AtomicBool::new(false));
    let mut turn_boundary_id = 0u64;

    let turn_start_idx = accepted.turn_start_idx;
    let user_msg_id = accepted.message_id;
    let prepared = accepted.prepared;
    let turn_started = std::time::Instant::now();
    let turn_start_cwd = ctx.session.cwd.clone();
    forward_terminal.store(false, Ordering::Release);
    if let Ok(mut capture) = turn_capture.capture.lock() {
        capture.terminal = None;
        capture.usage = TurnUsageCapture::default();
    }
    for plugin in &ctx.plugins {
        plugin.on_turn_started(&mut ctx.session, turn_start_idx);
    }

    let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));
    execute_turn_async(
        &mut session,
        &user_msg_id,
        &prepared,
        &mut ctx,
        &stream_tx,
        &mut cmd_rx,
    )
    .await;
    ctx.session = session;

    wait_for_stream_boundary(&stream_tx, &turn_capture, &mut turn_boundary_id).await;
    let mut terminal = turn_capture
        .capture
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .terminal
        .take()
        .unwrap_or(StreamEvent::Done { usage: None });

    if ctx.session.cwd != turn_start_cwd {
        let workspace_path = std::path::PathBuf::from(&ctx.session.cwd);
        let workspace = workspace_path.is_dir().then_some(workspace_path.as_path());
        for plugin in &ctx.plugins {
            plugin.set_workspace(workspace);
        }
    }

    let interrupted_tools =
        crate::react::message::close_unfinished_tool_calls_for_turn(&mut ctx.session);
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

    crate::react::message::flush_deferred_tool_injections(&mut ctx.session, &ctx);

    let elapsed_ms = turn_started.elapsed().as_millis() as u64;
    let mut status = TurnOutcome::from_terminal(&terminal).status;

    if status == tiangong_types::TurnStatus::Cancelled {
        for plugin in &ctx.plugins {
            plugin.on_cancel(&mut ctx.session).await;
        }
    }

    let mut user_msg_updated = false;
    if let Some(msg) = ctx
        .session
        .messages
        .iter_mut()
        .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
    {
        msg.set_turn_result(elapsed_ms, status);
        user_msg_updated = true;
    }
    for plugin in &ctx.plugins {
        plugin.on_turn_finished(&mut ctx.session, turn_start_idx);
    }

    wait_for_stream_boundary(&stream_tx, &turn_capture, &mut turn_boundary_id).await;
    let turn_usage = {
        let mut capture = turn_capture
            .capture
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::mem::take(&mut capture.usage)
    };
    turn_usage.apply_to_session(&mut ctx.session);
    ctx.session.clear_transient_content();

    if let Err(error) = ctx.session.try_persist_to_disk() {
        terminal = StreamEvent::Error {
            message: format!("最终会话持久化失败：{error}"),
        };
        status = tiangong_types::TurnStatus::Failed;
        if let Some(msg) = ctx
            .session
            .messages
            .iter_mut()
            .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
        {
            msg.set_turn_result(elapsed_ms, status);
        }
        let _ = ctx.session.try_persist_to_disk();
    }
    if user_msg_updated {
        crate::react::message::emit_session_message_upsert(&ctx.session, &ctx, &user_msg_id);
    }

    send_final_stream_event(
        &stream_tx,
        &forward_terminal,
        &turn_capture,
        &mut turn_boundary_id,
        terminal,
    )
    .await;

    drop(ctx);
}

pub(crate) fn reset_context_for_session(
    session: &mut Session,
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
    let _ = ctx.stream_tx.send(StreamEvent::ContextCompressed {
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
}

#[cfg(test)]
mod shared_runtime_tests {
    use super::*;
    use crate::core_config::{CoreConfig, CoreConfigProvider};

    /// 多个空闲 Core 不创建 turn task，且可正常关闭。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiple_cores_share_runtime_and_shutdown_cleanly() {
        let root = tempfile::tempdir().unwrap();

        let config = CoreConfigProvider::new(CoreConfig::default());
        let cores: Vec<TiangongCore> = (0..3)
            .map(|i| {
                let mut session = Session::new(format!("shared-runtime-{i}"));
                session.bind_storage_root(root.path());
                session.try_persist_to_disk().unwrap();
                let (event_tx, _event_rx) = std::sync::mpsc::channel();
                TiangongCore::builder()
                    .session_id(session.id.clone())
                    .config(config.clone())
                    .trust_mode(session.trust_mode)
                    .storage_root(root.path())
                    .event_sender(event_tx)
                    .build()
                    .unwrap()
            })
            .collect();

        // 空闲 Core 没有活跃 turn task。
        for core in &cores {
            assert!(core.is_stopped(), "空闲 Core 不应存在 turn task");
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
/// 时为 Cancelled，否则为 Failed。run_turn 据此把 `status` 与执行时长
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
    ctx: &mut crate::turn_context::TurnContext,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) {
    ctx.execute_turn(session, Some((message_id, prepared)), cmd_rx)
        .await;
}
