//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::sync::Arc;
use std::sync::mpsc::Sender;
use typed_builder::TypedBuilder;

use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::model::SingleProviderClient;
use crate::react::turn::run_turn;
use crate::session::Session;
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
                            return Err(CoreError::WorkerPanicked);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }

                let session_id = self.session_id.clone();
                let mut session = Session::load_from_storage(&self.storage_root, &session_id)
                    .map_err(|error| {
                        tracing::warn!(%error, %session_id, "加载本轮 Session 失败");
                        CoreError::WorkerStopped
                    })?;
                let stream_tx = self.stream_tx.clone();
                crate::react::message::accept_prepared_user_message_with_options(
                    &mut session,
                    &stream_tx,
                    message_id,
                    prepared,
                    true,
                )
                .map_err(|error| {
                    tracing::warn!(%error, "持久化本轮用户消息失败");
                    CoreError::WorkerStopped
                })?;
                let trust_mode = *self.trust_mode.lock().unwrap_or_else(|p| p.into_inner());
                let config = (*self.config.snapshot()).clone();
                let storage_root = self.storage_root.clone();
                let plugins = self.plugins.clone();
                // stream_tx 直接用 core 的通道(发 StreamEvent 到 app 层)

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
                let ctx = crate::turn_context::TurnContext::builder()
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

                crate::shared_runtime::spawn_turn(ctx, move |mut ctx, cmd_rx| {
                    // 每轮都触发 on_session_ready:插件自行保证幂等性(如 index 扫描的 last_scanned
                    // 标志、memory 的 session_id 已设置检查、agent-team 的 manifest 已存在检查)。
                    for plugin in &ctx.plugins {
                        plugin.on_session_ready(&mut ctx.session);
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
                    Ok(run_turn(ctx, cmd_rx))
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
