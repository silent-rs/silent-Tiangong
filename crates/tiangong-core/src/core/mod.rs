//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use typed_builder::TypedBuilder;

use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::model::SingleProviderClient;
use crate::react::turn::run_turn;
use crate::session::Session;
use crate::turn_context::TurnContext;
use tiangong_types::StreamEvent;

pub mod command;
pub(crate) use command::Command;
pub mod plugin;
pub use plugin::Plugin;
pub mod error;
pub mod storage_location;

pub use error::CoreError;
pub use storage_location::CoreStorageLocation;

/// 判断标题是否仍是默认值（"新对话"/"会话 X"）。
///
/// 用于 lite 自动生成标题写回时，避免覆盖用户已手动改过的标题。
pub(crate) fn is_default_title(title: &str) -> bool {
    title == "新对话" || title.starts_with("会话 ")
}

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
    /// 新对话创建后固定使用的工作目录。
    #[builder(setter(into))]
    workspace_dir: String,
    /// 事件输出通道(会话级,跨 turn;clone 给每个 turn task 的 forwarder)。
    stream_tx: Sender<StreamEvent>,
    /// 进程内插件(会话级,跨 turn)。
    plugins: Vec<Arc<dyn Plugin>>,
    /// on_session_ready 是否已对本 Core 实例执行过（会话级一次性状态，不落盘）。
    /// 首次 turn 置为 true，此后复用同一 Core 的轮次只触发 on_turn_started。
    #[builder(default)]
    session_ready: AtomicBool,
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
        // Core 和插件立即使用新值；Session 字段由活跃 turn 或下一轮统一写入。
        *self.trust_mode.lock().unwrap_or_else(|p| p.into_inner()) = mode;
        for plugin in &self.plugins {
            plugin.set_trust_mode(mode);
        }
        let _ = self.send_cmd(Command::SetTrustMode(mode));
    }

    /// 设置会话思考强度。
    ///
    /// 已经发出的模型请求不变；活跃 turn 会在下一次构建模型请求时使用新值。
    pub fn set_reasoning_effort(&self, effort: String) {
        self.config
            .update(|config| config.reasoning_effort = effort.clone());
        let _ = self.send_cmd(Command::SetReasoningEffort(effort));
    }

    /// 收集当前 Core 全部插件贡献的 @提及候选（native + WASM 统一经此聚合）。
    ///
    /// 调用时实时遍历 `self.plugins`（lazy 收集，不预存）：native 插件直接调
    /// `MentionCandidateProvider::mention_candidates`，WASM 插件经 adapter 桥接到
    /// 同一 trait。宿主（src-tauri 的 `get_mention_candidates` 命令）经 CoreManager
    /// 调用本方法，不再硬编码 skill/mcp。
    pub fn get_mentions(&self) -> Vec<crate::MentionCandidate> {
        self.plugins
            .iter()
            .flat_map(|plugin| plugin.mention_candidates())
            .collect()
    }

    /// 更新会话标题。用于用户手动编辑（不可放弃）。
    ///
    /// 与 trust_mode 一致的双分支协调：
    /// - turn 进行中：发 `Command::SetTitle`，由 turn task 在命令分支写入 `ctx.session`，
    ///   turn 结束 run_turn 统一落盘（避免与 turn 对 session 的读写竞争）。
    /// - Core 空闲：Core 是 session 权威持有者且无并发 turn，直接 load+改+persist。
    ///
    /// `only_if_default=true` 时仅当当前标题仍是默认值才覆盖（用于 lite 自动生成，
    /// 但那条路径直接走 `shared_runtime::send_command`，不经过此方法）；用户手动编辑传 false。
    pub fn set_title(&self, title: String, only_if_default: bool) -> Result<(), CoreError> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(CoreError::WorkerStopped);
        }
        if crate::shared_runtime::is_running(&self.session_id) {
            self.send_cmd(Command::SetTitle {
                title,
                only_if_default,
            })
        } else {
            let mut session = self.load_session()?;
            if only_if_default && !is_default_title(&session.title) {
                return Ok(());
            }
            session.title = title.clone();
            session.updated_at = tiangong_types::now_text();
            session
                .try_persist_to_disk()
                .map_err(|_| CoreError::WorkerStopped)?;
            // 空闲时也发 TitleChanged，让前端统一经事件更新标题（手动改空闲会话同样通知）。
            let _ = self.stream_tx.send(StreamEvent::TitleChanged { title });
            Ok(())
        }
    }

    fn load_session(&self) -> Result<Session, CoreError> {
        match Session::load_from_storage(&self.storage_root, &self.session_id) {
            Ok(session) => Ok(session),
            Err(error)
                if !self
                    .storage_root
                    .join("sessions")
                    .join(format!("{}.json", self.session_id))
                    .exists() =>
            {
                let config = self.config.snapshot();
                let mut session = Session::new("新对话");
                session.id = self.session_id.clone();
                session.trust_mode = *self
                    .trust_mode
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                session.reasoning_effort = Some(config.reasoning_effort.clone());
                session.cwd = self.workspace_dir.clone();
                session.cwd_mode = crate::session::SessionCwdMode::Custom;
                session.bind_storage_root(self.storage_root.clone());
                session.try_persist_to_disk().map_err(|persist_error| {
                    tracing::warn!(
                        error = %persist_error,
                        session_id = %self.session_id,
                        "创建 Session 失败"
                    );
                    CoreError::WorkerStopped
                })?;
                tracing::debug!(
                    session_id = %self.session_id,
                    load_error = %error,
                    "首次加载创建 Session"
                );
                Ok(session)
            }
            Err(error) => {
                tracing::warn!(%error, session_id = %self.session_id, "加载 Session 失败");
                Err(CoreError::WorkerStopped)
            }
        }
    }

    /// 加载当前 Session，并根据 Core 当前配置构建本轮执行上下文。
    fn build_turn_context(&self) -> Result<TurnContext, CoreError> {
        let mut session = self.load_session()?;
        let trust_mode = *self.trust_mode.lock().unwrap_or_else(|p| p.into_inner());
        session.trust_mode = trust_mode;

        let config = self.config.snapshot();
        let stream_tx = self.stream_tx.clone();
        let plugins = self.plugins.clone();
        let retry_tx = stream_tx.clone();
        let on_retry: crate::model::OnRetryCallback =
            Arc::new(move |attempt, max_attempts, _delay_ms, error_text| {
                let _ = retry_tx.send(StreamEvent::Retry {
                    message: error_text.to_string(),
                    attempt,
                    max_attempts,
                });
            });
        let client = SingleProviderClient::new(config.llm.chat.clone()).with_on_retry(on_retry);
        let lite_client = config.llm.lite.clone().map(SingleProviderClient::new);
        let prepared_plugins =
            crate::core::plugin::prepare_plugins(&plugins, &config, trust_mode, &session);

        Ok(TurnContext::builder()
            .client(client)
            .lite_client(lite_client)
            .session(session)
            .stream_tx(stream_tx)
            .plugins(plugins)
            .context_limit(config.context_limit)
            .agent_config(crate::agent_config::AgentConfig {
                trust_mode,
                default_trust_mode: config.default_trust_mode,
                custom_system_prompt: config.custom_system_prompt.clone(),
                reasoning_effort: config.reasoning_effort.clone(),
            })
            .trust_mode(trust_mode)
            .observer(crate::observe::Observer::new(self.storage_root.clone()))
            .tool_overrides(prepared_plugins.tool_overrides)
            .tools(prepared_plugins.tools)
            .build())
    }

    fn compress_context(&self) -> Result<(), CoreError> {
        if self.is_busy() {
            return Err(CoreError::Busy);
        }

        let ctx = self.build_turn_context()?;

        crate::shared_runtime::spawn_turn(ctx, move |ctx, cmd_rx| {
            Ok(crate::react::compression::run_manual_context_compression(
                ctx, cmd_rx,
            ))
        })
    }

    fn reset_context(&self) -> Result<(), CoreError> {
        if self.is_busy() {
            return Err(CoreError::Busy);
        }

        let mut session = self.load_session()?;
        let total = session.messages.len();
        session.summary_up_to = total;
        crate::context::compressor::mark_compact_boundary(&mut session.messages, total);
        session.context_summary = None;
        session.current_tokens = 0;
        session.active_agent_current_tokens = 0;
        session.agent_current_tokens.clear();
        session.try_persist_to_disk().map_err(|error| {
            tracing::warn!(%error, session_id = %self.session_id, "清空上下文落盘失败");
            CoreError::WorkerStopped
        })?;
        crate::react::compression::notify_cleared(&self.stream_tx, &session);
        Ok(())
    }

    /// 关闭并获取最终 session。
    ///
    /// worker panic 时返回 [`CoreError::WorkerPanicked`]，不再静默兜底为
    /// `Session::new("recovered")`——避免丢失原会话数据后调用方误判成功。
    /// 从磁盘加载最终 session。
    pub fn into_session(self) -> Result<Session, CoreError> {
        self.finalize_session()
    }

    /// 关闭 Core,不取回 session(session 已在磁盘上)。
    pub fn shutdown_join(self) -> Result<(), CoreError> {
        self.finalize_session().map(|_| ())
    }

    fn finalize_session(&self) -> Result<Session, CoreError> {
        crate::shared_runtime::cancel_and_join(&self.session_id)?;
        // 关闭语义：丢弃封口期间排队的待启动消息（后台自动启动任务会 drain 到空并退出，
        // 不会在会话关闭后偷偷启动新轮）。
        crate::shared_runtime::clear_next_turn(&self.session_id);
        let mut session = self.load_session()?;
        self.finalize_plugins(&mut session);
        session.try_persist_to_disk().map_err(|error| {
            tracing::warn!(%error, session_id = %self.session_id, "持久化会话结束钩子结果失败");
            CoreError::WorkerStopped
        })?;
        Ok(session)
    }

    /// 遍历插件调用 on_session_ended（worker 退出前的 finalize 钩子）。
    fn finalize_plugins(&self, session: &mut Session) {
        for plugin in &self.plugins {
            plugin.on_session_ended(session);
        }
    }

    /// 逐条保存并确认下一轮队列中的用户消息；返回成功消费的条数。
    /// 任一保存失败：返回 `Err((已消费条数, 错误))`，调用方按位置放回未处理部分
    /// （**保存成功才确认/推进**，失败不虚报、不丢消息，ALR-202）。
    fn save_and_confirm_pending(
        ctx: &mut TurnContext,
        pending: &[Command],
    ) -> Result<usize, (usize, String)> {
        let mut idx = 0;
        while idx < pending.len() {
            let Command::InjectUserMessage {
                message_id,
                content,
            } = &pending[idx]
            else {
                idx += 1;
                continue;
            };
            let content_text = tiangong_types::content_blocks_text(content);
            let content_blocks = tiangong_types::stable_content_blocks(content);
            let event_id = message_id.clone();
            ctx.session
                .try_append_prepared_user_message_with_id(message_id.clone(), content.clone())
                .map_err(|error| (idx, error))?;
            let _ = ctx.stream_tx.send(StreamEvent::UserMessage {
                message_id: event_id,
                content: content_text,
                content_blocks,
                media: Vec::new(),
                model_excluded: false,
            });
            idx += 1;
        }
        Ok(idx)
    }

    /// 构建系统提示并启动 turn（含首次 on_session_ready）。
    fn spawn_next_turn(first_turn: bool, ctx: TurnContext) -> Result<(), CoreError> {
        crate::shared_runtime::spawn_turn(ctx, move |mut ctx, cmd_rx| {
            // on_session_ready 仅在本 Core 实例的首次 turn 执行（会话级一次性初始化）。
            if first_turn {
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
            Ok(run_turn(ctx, cmd_rx))
        })
    }

    /// 消费已取出的下一轮队列消息并启动 turn（后台自动启动与常规 deliver 共用）。
    ///
    /// 顺序保证：**逐条保存成功才确认**（失败放回未处理部分）；随后 spawn；spawn 失败
    /// （其他启动者已抢先 spawn 时 Busy）把已保存消息**注入到已运行轮次**，注入失败的
    /// 放回队列前端（幂等重放）——任何路径都不丢消息、不虚报成功。
    fn consume_next_turn_and_start(
        sid: &str,
        mut ctx: TurnContext,
        mut pending: Vec<Command>,
        first_turn: bool,
    ) -> Result<(), CoreError> {
        let consumed = match Self::save_and_confirm_pending(&mut ctx, &pending) {
            Ok(consumed) => consumed,
            Err((consumed, error)) => {
                let remaining = pending.split_off(consumed);
                let restored = crate::shared_runtime::requeue_next_turn_front(sid, remaining);
                tracing::warn!(
                    %error,
                    restored,
                    session_id = sid,
                    "下一轮队列消息保存失败，未处理部分已放回队列"
                );
                return Err(CoreError::WorkerStopped);
            }
        };
        if let Err(error) = Self::spawn_next_turn(first_turn, ctx) {
            // 其他启动者已抢先 spawn（Busy）或注册表异常：已保存消息未被当前轮看到。
            // 尝试注入到已运行轮次（ALR-101 同 turn 重启）；注入失败则放回队列待下次消费。
            tracing::warn!(
                %error,
                session_id = sid,
                "消费队列后启动 turn 失败，尝试把已保存消息注入到已运行轮次"
            );
            let mut requeued: Vec<Command> = Vec::new();
            for cmd in &pending[..consumed] {
                let Command::InjectUserMessage {
                    message_id,
                    content,
                } = cmd
                else {
                    continue;
                };
                let injected = crate::shared_runtime::send_command(
                    sid,
                    Command::InjectUserMessage {
                        message_id: message_id.clone(),
                        content: content.clone(),
                    },
                );
                if !injected {
                    requeued.push(Command::InjectUserMessage {
                        message_id: message_id.clone(),
                        content: content.clone(),
                    });
                }
            }
            if !requeued.is_empty() {
                let restored = crate::shared_runtime::requeue_next_turn_front(sid, requeued);
                tracing::warn!(
                    restored,
                    session_id = sid,
                    "注入失败的消息已放回队列前端（幂等重放，不丢消息）"
                );
            }
            return Err(error);
        }
        Ok(())
    }

    /// 封口期间的用户消息：**入队即接受** + 后台自动启动下一轮。
    ///
    /// 先构建上下文（失败则消息不入队，返回错误，不"失败但保留"）；入队成功即返回
    /// Ok（已接受），由后台任务在旧轮结束后消费队列并启动下一轮——不同步轮询等待、
    /// 不阻塞调用线程与 Core Manager 注册表锁（ALR-202）；会话关闭时队列被清空，
    /// 后台任务 drain 为空直接退出（不会在关闭后偷偷启动）。
    fn queue_next_turn_and_auto_start(
        &self,
        msg_id: String,
        prepared: Vec<tiangong_types::ContentBlock>,
    ) -> Result<(), CoreError> {
        let ctx = self.build_turn_context()?;
        let queued = crate::shared_runtime::push_next_turn(
            &self.session_id,
            Command::InjectUserMessage {
                message_id: msg_id,
                content: prepared,
            },
        );
        if !queued {
            tracing::warn!(
                session_id = %self.session_id,
                "注入用户消息入队下一轮失败（内部锁异常）"
            );
            return Err(CoreError::WorkerStopped);
        }
        tracing::debug!(
            session_id = %self.session_id,
            "注入用户消息时 turn 正在封口，已排入下一轮队列，后台自动启动"
        );
        let sid = self.session_id.clone();
        crate::shared_runtime::shared_runtime().spawn(async move {
            // 等待旧轮结束（异步轮询，不阻塞任何调用线程/注册表锁）。
            while crate::shared_runtime::is_running(&sid) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            // 会话关闭时队列被清空：drain 为空则直接退出，不在关闭后偷偷启动。
            let pending = match crate::shared_runtime::drain_next_turn(&sid) {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        session_id = %sid,
                        "下一轮队列读取失败，消息保留待下次触发"
                    );
                    return;
                }
            };
            if pending.is_empty() {
                return;
            }
            // 消费队列并启动（后台启动的是后续轮次，不做会话级 on_session_ready，
            // 只依赖 on_turn_started 处理每轮增量）。
            if let Err(error) = Self::consume_next_turn_and_start(&sid, ctx, pending, false) {
                tracing::warn!(
                    %error,
                    session_id = %sid,
                    "后台自动启动下一轮未完成，消息已放回队列或注入已运行轮次"
                );
            }
        });
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
                // 有活跃 turn task 时注入用户消息：中断主循环直接拥有的活动，在同一
                // 物理 turn 内保存新消息并从新意图重启（ALR-101），避免取消 + 重新
                // spawn 带来的生命周期重复触发和插件副作用。
                if crate::shared_runtime::is_running(&self.session_id) {
                    let msg_id = message_id.clone().unwrap_or_else(scru128::new_string);
                    let sent = crate::shared_runtime::send_command(
                        &self.session_id,
                        Command::InjectUserMessage {
                            message_id: msg_id.clone(),
                            content: prepared.clone(),
                        },
                    );
                    if sent {
                        // 不在此处发送 UserMessage 确认事件：由执行线程在校验并成功
                        // 保存消息后再发送——命令成功写入通道不等于已处理。
                        return Ok(());
                    }
                    // 发送失败：turn 正在终态封口（Sealing/Committing）。**入队即接受**：
                    // 消息进入下一轮队列，由后台任务在旧轮结束后自动启动并消费（不依赖
                    // 用户再发消息、不同步轮询等待、不阻塞调用线程与注册表锁）；构建
                    // 上下文或入队失败时如实返回错误，不虚报成功（ALR-202）。
                    return self.queue_next_turn_and_auto_start(msg_id, prepared);
                }

                // 无活跃 turn：常规路径。合并封口期间遗留的下一轮队列消息与本轮消息，
                // 逐条保存确认后启动 turn（队列读取失败时消息保留，如实返回错误）。
                let ctx = self.build_turn_context()?;
                let pending =
                    crate::shared_runtime::drain_next_turn(&self.session_id).map_err(|error| {
                        tracing::warn!(
                            %error,
                            session_id = %self.session_id,
                            "下一轮队列读取失败，消息保留待下次触发"
                        );
                        error
                    })?;
                let message_id = message_id.unwrap_or_else(scru128::new_string);
                let mut all = pending;
                all.push(Command::InjectUserMessage {
                    message_id,
                    content: prepared,
                });
                let first_turn = self
                    .session_ready
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok();
                Self::consume_next_turn_and_start(&self.session_id, ctx, all, first_turn)
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
                CommandInput::SetTrustMode(trust_mode) => {
                    self.set_trust_mode(trust_mode);
                    Ok(())
                }
                CommandInput::CompressContext => self.compress_context(),
                CommandInput::ResetContext => self.reset_context(),
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

#[cfg(test)]
mod shared_runtime_tests {
    use super::*;
    use crate::core_config::{CoreConfig, CoreConfigProvider};

    struct MentionPlugin {
        id: &'static str,
        values: Vec<crate::MentionCandidate>,
    }

    impl crate::tool_override::ToolSpecProvider for MentionPlugin {}
    impl crate::tool_override::ToolOverrideHandler for MentionPlugin {}
    impl crate::tool_override::PromptSectionProvider for MentionPlugin {}
    impl crate::tool_override::MentionCandidateProvider for MentionPlugin {
        fn mention_candidates(&self) -> Vec<crate::MentionCandidate> {
            self.values.clone()
        }
    }
    impl Plugin for MentionPlugin {
        fn id(&self) -> &str {
            self.id
        }
    }

    #[test]
    fn get_mentions_aggregates_all_plugin_candidates() {
        let root = tempfile::tempdir().unwrap();
        let (event_tx, _event_rx) = std::sync::mpsc::channel();
        let candidate = |value: &str| crate::MentionCandidate {
            value: value.to_string(),
            label: value.to_string(),
            kind: "test".to_string(),
            hint: String::new(),
        };
        let core = TiangongCore::builder()
            .session_id("mention-test")
            .config(CoreConfigProvider::new(CoreConfig::default()))
            .trust_mode(crate::permission::TrustMode::FullTrust)
            .storage_root(root.path())
            .workspace_dir(root.path().to_string_lossy())
            .stream_tx(event_tx)
            .plugins(vec![
                Arc::new(MentionPlugin {
                    id: "one",
                    values: vec![candidate("@one")],
                }),
                Arc::new(MentionPlugin {
                    id: "two",
                    values: vec![candidate("@two")],
                }),
            ])
            .build();

        let values = core
            .get_mentions()
            .into_iter()
            .map(|item| item.value)
            .collect::<Vec<_>>();
        assert_eq!(values, vec!["@one", "@two"]);
    }

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
                    .workspace_dir(root.path().to_string_lossy())
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

    /// ALR-202 交接端到端：封口窗口到达的用户消息——**入队即接受**（快速返回、
    /// 不同步等待、不阻塞其他会话）、**只确认一次**、旧轮结束后**后台自动启动
    /// 下一轮**并持久化；关闭时队列被清空，消息不会在未来偷偷执行。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sealing_window_message_auto_starts_next_turn_with_single_confirmation() {
        use crate::agent_input::{AgentInput, AgentInputKind};
        use crate::session::Session;

        let root = tempfile::tempdir().unwrap();
        let config = CoreConfigProvider::new(CoreConfig::default());

        // 会话 A：主测试对象。
        let mut session_a = Session::new("seal-a");
        session_a.bind_storage_root(root.path());
        session_a.try_persist_to_disk().unwrap();
        let (event_tx_a, event_rx_a) = std::sync::mpsc::channel::<StreamEvent>();
        let core_a = TiangongCore::builder()
            .session_id(session_a.id.clone())
            .config(config.clone())
            .trust_mode(session_a.trust_mode)
            .storage_root(root.path())
            .workspace_dir(root.path().to_string_lossy())
            .stream_tx(event_tx_a)
            .plugins(vec![])
            .build();

        // 会话 B：验证 A 的封口交接不阻塞其他会话。
        let mut session_b = Session::new("seal-b");
        session_b.bind_storage_root(root.path());
        session_b.try_persist_to_disk().unwrap();
        let (event_tx_b, _event_rx_b) = std::sync::mpsc::channel::<StreamEvent>();
        let core_b = TiangongCore::builder()
            .session_id(session_b.id.clone())
            .config(config.clone())
            .trust_mode(session_b.trust_mode)
            .storage_root(root.path())
            .workspace_dir(root.path().to_string_lossy())
            .stream_tx(event_tx_b)
            .plugins(vec![])
            .build();

        let sid = session_a.id.clone();
        // 挂起一个旧轮（release 控制结束），并让注册表按 A 的 session_id 记录。
        let (mut ctx, _) = crate::shared_runtime::dummy_context();
        ctx.session.id = sid.clone();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        crate::shared_runtime::spawn_turn(ctx, move |_ctx, cmd_rx| {
            Ok(async move {
                let _cmd_rx = cmd_rx;
                let _ = release_rx.await;
            })
        })
        .expect("spawn 旧轮失败");

        // 封口：旧轮进入 Sealing，新命令被拒。
        crate::shared_runtime::begin_seal(&sid);
        assert!(
            !crate::shared_runtime::send_command(&sid, Command::Cancel),
            "Sealing 后应拒绝投递"
        );

        // A.deliver 用户消息：入队即接受，快速返回 Ok（不进入同步等待）。
        let msg_id = "seal-msg-1".to_string();
        let start = std::time::Instant::now();
        core_a
            .deliver(AgentInputKind::prepared_with_id(
                msg_id.clone(),
                vec![tiangong_types::ContentBlock::text("封口期间消息")],
            ))
            .expect("封口路径应返回 Ok（入队即接受）");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "入队即接受，不应同步轮询等待旧轮结束"
        );
        assert!(
            crate::shared_runtime::has_next_turn(&sid),
            "消息应已进入下一轮队列"
        );

        // B 不受阻塞：A 的旧轮仍挂起（封口状态），B 投递快速返回。
        let start_b = std::time::Instant::now();
        core_b
            .deliver(AgentInputKind::message("B 的消息"))
            .expect("B 投递不应受 A 封口交接影响");
        assert!(
            start_b.elapsed() < std::time::Duration::from_secs(1),
            "其他会话不应被 A 的封口交接阻塞"
        );

        // 释放旧轮：后台任务应自动消费队列并启动下一轮。
        release_tx.send(()).expect("释放旧轮失败");
        // 等待确认事件出现（后台任务保存消息后发出）——不能只等 is_running，
        // 后台任务与测试的轮询存在时序窗口，需以事件为交接完成信号。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut user_message_count = 0usize;
        loop {
            while let Ok(event) = event_rx_a.try_recv() {
                if let StreamEvent::UserMessage { message_id, .. } = event {
                    assert_eq!(message_id, msg_id, "确认事件应使用原消息 ID");
                    user_message_count += 1;
                }
            }
            if user_message_count >= 1 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "等待后台交接确认超时");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // 等待后台任务彻底完成：新轮（LLM 不可达）快速结束并清理注册表，队列已空。
        while (crate::shared_runtime::is_running(&sid)
            || crate::shared_runtime::has_next_turn(&sid))
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // 只确认一次：交接完成后 UserMessage 事件总数恰好 1（后台任务保存时发出，
        // 不重复确认；旧轮与新轮都不再发同 ID 确认）。
        while let Ok(event) = event_rx_a.try_recv() {
            if let StreamEvent::UserMessage { message_id, .. } = event {
                assert_eq!(message_id, msg_id, "确认事件应使用原消息 ID");
                user_message_count += 1;
            }
        }
        assert_eq!(user_message_count, 1, "封口消息应只确认一次，不重复发送");

        // 消息已持久化到磁盘 session。
        let loaded = core_a.load_session().expect("加载 session 失败");
        assert!(
            loaded.messages.iter().any(|m| m.id == msg_id),
            "封口消息应已保存到 session"
        );
        assert!(
            !crate::shared_runtime::has_next_turn(&sid),
            "队列消费后应为空"
        );

        // 关闭路径：清空待启动队列，防止已接受消息在会话关闭后偷偷执行。
        crate::shared_runtime::push_next_turn(&sid, Command::Cancel);
        assert!(crate::shared_runtime::has_next_turn(&sid));
        core_a.shutdown_join().expect("关闭 A 失败");
        assert!(
            !crate::shared_runtime::has_next_turn(&sid),
            "关闭后待启动队列应被清空"
        );
        core_b.shutdown_join().expect("关闭 B 失败");
    }
}
