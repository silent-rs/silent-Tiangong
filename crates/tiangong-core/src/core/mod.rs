//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use tokio::sync::mpsc as tokio_mpsc;
use typed_builder::TypedBuilder;

use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::model::SingleProviderClient;
use crate::react::message::{AcceptedUserMessage, accept_user_message};
use crate::session::{MessageRole, Session};
use tiangong_types::StreamEvent;

pub mod command;
pub(crate) use command::Command;
pub mod plugin;
pub use plugin::Plugin;
pub mod error;
pub mod storage_location;

pub use error::CoreError;
pub use storage_location::CoreStorageLocation;

/// 天工智能体核心
#[derive(TypedBuilder)]
#[builder(
    builder_method(vis = "pub"),
    builder_type(vis = "pub"),
    build_method(vis = "pub")
)]
pub struct TiangongCore {
    /// 会话 ID
    #[builder(setter(into))]
    session_id: String,
    /// 当前 Core 独立的配置提供者。
    config: CoreConfigProvider,
    /// 会话信任模式。
    #[builder(setter(transform = |mode: crate::permission::TrustMode| std::sync::Mutex::new(mode)))]
    trust_mode: std::sync::Mutex<crate::permission::TrustMode>,
    /// 存储根目录(turn task 从此加载/保存 session)。
    #[builder(setter(into))]
    storage_root: std::path::PathBuf,
    /// 事件输出通道(会话级,跨 turn;clone 给每个 turn task 的 forwarder)。
    stream_tx: Sender<StreamEvent>,
    /// 进程内插件(会话级,跨 turn)。
    plugins: Vec<Arc<dyn Plugin>>,
    /// on_session_ready 是否已触发(跨 turn)。
    #[builder(default = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)))]
    session_ready_fired: Arc<std::sync::atomic::AtomicBool>,
}

impl TiangongCore {
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
                    .stream_tx(stream_tx.clone())
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
                let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));
                let accepted =
                    accept_user_message(&mut session, &ctx, Some(message_id), prepared, true)
                        .map_err(|error| {
                            tracing::warn!(%error, "持久化本轮用户消息失败");
                            CoreError::WorkerStopped
                        })?;
                ctx.session = session;

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
                    Ok(run_turn(ctx, cmd_rx, accepted))
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
                CommandInput::SetTrustMode(truest_mod) => {
                    self.trust_mode
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone_from(&truest_mod);
                    self.send_cmd(Command::SetTrustMode(truest_mod))
                }
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
    accepted: AcceptedUserMessage,
) {
    let stream_tx = ctx.stream_tx.clone();
    let turn_start_idx = accepted.turn_start_idx;
    let user_msg_id = accepted.message_id;
    let prepared = accepted.prepared;
    let turn_started = std::time::Instant::now();
    let turn_start_cwd = ctx.session.cwd.clone();

    for plugin in &ctx.plugins {
        plugin.on_turn_started(&mut ctx.session, turn_start_idx);
    }

    let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));
    let usage = ctx
        .execute_turn(&mut session, Some((&user_msg_id, &prepared)), &mut cmd_rx)
        .await;
    ctx.session = session;

    // execute_turn 返回的 usage 直接应用到 session
    ctx.session.token_usage.accumulate(&usage);

    let mut terminal = StreamEvent::Done { usage: None };

    if ctx.session.cwd != turn_start_cwd {
        let workspace_path = std::path::PathBuf::from(&ctx.session.cwd);
        let workspace = workspace_path.is_dir().then_some(workspace_path.as_path());
        for plugin in &ctx.plugins {
            plugin.set_workspace(workspace);
        }
    }

    let interrupted_tools = {
        let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));
        let result = crate::react::message::close_unfinished_tool_calls_for_turn(&mut session);
        ctx.session = session;
        result
    };
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
        terminal = StreamEvent::Error {
            message: "本轮仍有未完成的工具调用，已安全中断".to_string(),
        };
    }

    {
        let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));
        crate::react::message::flush_deferred_tool_injections(&mut session, &ctx);
        ctx.session = session;
    }

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

    // 直接发送终态事件(无 forwarder 屏障)
    let _ = stream_tx.send(terminal);

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
                    .stream_tx(event_tx)
                    .plugins(vec![])
                    .build()
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
