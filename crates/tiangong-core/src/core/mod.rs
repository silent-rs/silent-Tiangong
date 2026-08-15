//! TiangongCore：天工智能体核心
//!
//! 输入经 [`AgentInput::deliver`] 进入 Agent 自有 Inbox，由唯一 driver 顺序
//! 消费：每个 turn 真正开始时从最新 Session 构建上下文（ALR-201/204），
//! 执行模型—工具循环并提交（ALR-001~005）。
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use typed_builder::TypedBuilder;

use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::model::SingleProviderClient;
use crate::react::inbox::{AgentScheduling, CommandIngress, TurnInput};
use crate::react::turn::run_turn;
use crate::session::Session;
use crate::turn_context::TurnContext;
use tiangong_types::StreamEvent;

pub mod command;
pub(crate) use command::Command;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod integration_tests;
pub mod plugin;
#[cfg(test)]
pub(crate) mod test_support;
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
    #[builder(setter(transform = |mode: crate::permission::TrustMode| std::sync::Arc::new(std::sync::Mutex::new(mode))))]
    trust_mode: std::sync::Arc<std::sync::Mutex<crate::permission::TrustMode>>,
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
    session_ready: Arc<AtomicBool>,
}

impl TiangongCore {
    /// 向活跃 turn task 发送命令(无活跃 task 则忽略)。
    fn send_cmd(&self, cmd: Command) -> Result<(), CoreError> {
        crate::react::inbox::send_command(&self.session_id, cmd)
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

    /// 是否已停止（Agent 已关闭，无法再接收输入）。
    ///
    /// 空闲但未关闭的 Core 仍可接收输入（driver 挂起等待唤醒）。
    pub fn is_stopped(&self) -> bool {
        !crate::react::inbox::is_alive(&self.session_id)
    }

    /// 是否正在执行 turn（对外 `Running`：driver 正在执行模型/工具/压缩活动）。
    pub fn is_busy(&self) -> bool {
        crate::react::inbox::is_running(&self.session_id)
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
        if crate::react::inbox::is_running(&self.session_id) {
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
        self.turn_spawner().load_session()
    }

    /// 确保 Agent 调度实体与唯一 driver 存在（幂等；并发调用不会重复启动）。
    fn ensure_scheduling(&self) -> Result<Arc<AgentScheduling>, CoreError> {
        let spawner = self.turn_spawner();
        crate::react::inbox::ensure_agent_session(&self.session_id, move |scheduling| {
            drive_agent(scheduling, spawner)
        })
    }

    /// 构造 driver 每轮执行所需的材料快照（字段克隆，便宜且与 Core 解耦）。
    fn turn_spawner(&self) -> TurnSpawner {
        TurnSpawner {
            session_id: self.session_id.clone(),
            config: self.config.clone(),
            trust_mode: self.trust_mode.clone(),
            storage_root: self.storage_root.clone(),
            workspace_dir: self.workspace_dir.clone(),
            stream_tx: self.stream_tx.clone(),
            plugins: self.plugins.clone(),
            session_ready: self.session_ready.clone(),
        }
    }

    fn compress_context(&self) -> Result<(), CoreError> {
        if self.is_busy() {
            return Err(CoreError::Busy);
        }
        // 手动压缩作为控制输入进入 Inbox，由唯一 driver 在空闲时执行，
        // 与后续用户消息天然串行（不再独立 spawn）。
        let scheduling = self.ensure_scheduling()?;
        scheduling.deliver_input(TurnInput::ManualCompression)
    }

    fn reset_context(&self) -> Result<(), CoreError> {
        if self.is_busy() {
            return Err(CoreError::Busy);
        }
        let scheduling = self.ensure_scheduling()?;
        scheduling.deliver_input(TurnInput::ResetContext)
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
        // 关闭语义（ALR-206）：先停止接收新输入，取消当前轮并等待唯一 driver
        // 收敛；Inbox 中未处理的已确认消息由 driver 持久化到磁盘（可恢复），
        // 不静默丢弃，也不在关闭后启动新 turn。
        crate::react::inbox::shutdown_agent(&self.session_id)?;
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
}

impl crate::agent_input::AgentInput for TiangongCore {
    fn deliver(&self, input: crate::agent_input::AgentInputKind) -> Result<(), CoreError> {
        use crate::agent_input::{AgentInputKind, ApprovalInput, CommandInput, MessageInput};

        match input {
            AgentInputKind::Message(MessageInput::UserMessage {
                prepared,
                message_id,
            }) => {
                // 消息排队策略归 app 层：core 空闲时直接执行（单槽交接），
                // 运行中按引导注入当前轮；封口瞬间到达的占用单槽等当前轮
                // 结束。确认事件在 driver 校验并成功保存后才发送（ALR-102/202）。
                let msg_id = message_id.unwrap_or_else(scru128::new_string);
                let scheduling = self.ensure_scheduling()?;
                scheduling.push_steer(msg_id, prepared)
            }
            AgentInputKind::Tool(tool) => {
                // inject：不唤醒 driver；空闲期积压在 next_step，由下一个 turn
                // 开始时领取（ALR-102/103）。
                let scheduling = crate::react::inbox::ensure_inbox(&self.session_id)?;
                scheduling.push_inject(tool.tool_name().to_string(), tool.render())
            }
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
        // 非阻塞关闭：停止接收、取消当前轮并唤醒 driver 自行收敛退出（driver 会
        // 持久化 Inbox 中未处理的已确认消息）。不在此等待，避免 drop 路径阻塞。
        crate::react::inbox::detach_shutdown(&self.session_id);
    }
}

/// driver 每轮构建与执行 turn 所需的全部材料（从 TiangongCore 克隆）。
///
/// 每个 turn 真正开始时才调用 [`TurnSpawner::build_turn_context`] 从最新磁盘
/// Session 与最新配置构建上下文——driver 不持有任何跨 turn 的 Session 快照
/// （ALR-201/204）。
struct TurnSpawner {
    session_id: String,
    config: CoreConfigProvider,
    trust_mode: Arc<std::sync::Mutex<crate::permission::TrustMode>>,
    storage_root: std::path::PathBuf,
    workspace_dir: String,
    stream_tx: Sender<StreamEvent>,
    plugins: Vec<Arc<dyn Plugin>>,
    session_ready: Arc<AtomicBool>,
}

impl TurnSpawner {
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

    /// 加载最新 Session，并根据当前配置构建本轮执行上下文（ALR-204）。
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

    /// 执行一个用户消息 turn：保存消息 → 构建命令通道 → 运行 Agent Loop。
    ///
    /// 消息保存成功才发送 `UserMessage` 确认事件（ALR-202）；构建或保存失败时
    /// 发送明确错误事件并跳过本轮，不虚报成功。
    #[allow(clippy::too_many_arguments)]
    async fn run_user_turn(
        &self,
        scheduling: &AgentScheduling,
        message_id: String,
        content: Vec<tiangong_types::ContentBlock>,
        pending_steps: Vec<Command>,
    ) {
        let mut ctx = match self.build_turn_context() {
            Ok(ctx) => ctx,
            Err(error) => {
                tracing::warn!(%error, session_id = %self.session_id, "构建 turn 上下文失败");
                let _ = self.stream_tx.send(StreamEvent::Error {
                    message: format!("会话上下文构建失败：{error}"),
                });
                return;
            }
        };
        // on_session_ready 仅在本 Core 实例的首次 turn 执行（会话级一次性初始化）。
        if self
            .session_ready
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            for plugin in &ctx.plugins {
                plugin.on_session_ready(&mut ctx.session);
            }
        }
        // 保存成功才确认（ALR-202）；失败发明确错误并放弃本轮。
        if let Err(error) = ctx
            .session
            .try_append_prepared_user_message_with_id(message_id.clone(), content.clone())
        {
            tracing::warn!(%error, session_id = %self.session_id, "用户消息保存失败");
            let _ = self.stream_tx.send(StreamEvent::Error {
                message: format!("用户消息保存失败：{error}"),
            });
            return;
        }
        let _ = ctx.stream_tx.send(StreamEvent::UserMessage {
            message_id: message_id.clone(),
            content: tiangong_types::content_blocks_text(&content),
            content_blocks: tiangong_types::stable_content_blocks(&content),
            media: Vec::new(),
            model_excluded: false,
        });
        let prompt_sections = ctx
            .plugins
            .iter()
            .flat_map(|plugin| plugin.prompt_sections())
            .collect();
        ctx.session.rebuild_system_prompt(
            &crate::prompt::SystemPromptConfig::from_plugin_sections(prompt_sections),
        );
        if let Err(error) = ctx.session.try_persist_to_disk() {
            tracing::warn!(%error, "持久化本轮系统提示失败");
        }

        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let ingress = CommandIngress::new(cmd_tx);
        for plugin in &ctx.plugins {
            plugin.set_feedback_tx(crate::core::plugin::PluginFeedbackTx::new(ingress.clone()));
        }
        // turn 开始时领取的 next_step（inject）先于模型请求进入本轮（ALR-103）。
        for cmd in pending_steps {
            let _ = ingress.send(cmd);
        }
        scheduling.begin_turn(ingress);
        run_turn(ctx, cmd_rx).await;
        scheduling.end_turn();
    }

    /// 空闲期手动上下文压缩（作为 Inbox 控制输入由 driver 执行）。
    async fn run_manual_compression(&self, scheduling: &AgentScheduling) {
        let ctx = match self.build_turn_context() {
            Ok(ctx) => ctx,
            Err(error) => {
                tracing::warn!(%error, session_id = %self.session_id, "构建压缩上下文失败");
                let _ = self.stream_tx.send(StreamEvent::Error {
                    message: format!("压缩上下文构建失败：{error}"),
                });
                return;
            }
        };
        let mut ctx = ctx;
        // 防御性重建 system prompt：压缩请求需要它承载旧摘要（裸 session 直接
        // 手动压缩时可能缺失）。
        let prompt_sections = ctx
            .plugins
            .iter()
            .flat_map(|plugin| plugin.prompt_sections())
            .collect();
        ctx.session.rebuild_system_prompt(
            &crate::prompt::SystemPromptConfig::from_plugin_sections(prompt_sections),
        );
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let ingress = CommandIngress::new(cmd_tx);
        for plugin in &ctx.plugins {
            plugin.set_feedback_tx(crate::core::plugin::PluginFeedbackTx::new(ingress.clone()));
        }
        scheduling.begin_turn(ingress);
        // 压缩独立于用户意图：期间到达的新输入（引导消息/工具注入）在压缩
        // 等待循环内即时转入 Inbox 排队，压缩继续完成；只有终止类命令或
        // 转排队失败（会话关闭）才会取消压缩，此处无需再处理上抛。
        let defer_input = |command: Command| -> Result<(), Command> {
            let queued = match &command {
                Command::InjectUserMessage {
                    message_id,
                    content,
                } => scheduling
                    .deliver_input(crate::react::inbox::TurnInput::UserMessage {
                        message_id: message_id.clone(),
                        content: content.clone(),
                    })
                    .is_ok(),
                Command::InjectTool { tool_name, payload } => scheduling
                    .push_inject(tool_name.clone(), payload.clone())
                    .is_ok(),
                _ => false,
            };
            if queued { Ok(()) } else { Err(command) }
        };
        let _ =
            crate::react::compression::run_manual_context_compression(ctx, cmd_rx, &defer_input)
                .await;
        scheduling.end_turn();
    }

    /// 空闲期清理上下文（同步执行，不涉及模型请求）。
    fn run_reset_context(&self) {
        let mut session = match self.load_session() {
            Ok(session) => session,
            Err(error) => {
                let _ = self.stream_tx.send(StreamEvent::Error {
                    message: format!("加载会话失败：{error}"),
                });
                return;
            }
        };
        let total = session.messages.len();
        session.summary_up_to = total;
        crate::context::compressor::mark_compact_boundary(&mut session.messages, total);
        session.context_summary = None;
        session.current_tokens = 0;
        session.active_agent_current_tokens = 0;
        session.agent_current_tokens.clear();
        if let Err(error) = session.try_persist_to_disk() {
            tracing::warn!(%error, session_id = %self.session_id, "清空上下文落盘失败");
            let _ = self.stream_tx.send(StreamEvent::Error {
                message: format!("清空上下文落盘失败：{error}"),
            });
            return;
        }
        crate::react::compression::notify_cleared(&self.stream_tx, &session);
    }

    /// 关闭时持久化 Inbox 中未处理的已确认用户消息（ALR-206：可恢复，不静默丢弃）。
    ///
    /// 持久化成功发送 `UserMessage` 确认事件（消息已进入会话，用户重开可见）；
    /// 无法持久化时发送明确错误事件。控制输入（压缩/重置）直接丢弃，无用户数据。
    fn persist_pending_on_shutdown(&self, scheduling: &AgentScheduling) {
        let pending = scheduling.drain_pending();
        let user_messages: Vec<(String, Vec<tiangong_types::ContentBlock>)> = pending
            .into_iter()
            .filter_map(|input| match input {
                TurnInput::UserMessage {
                    message_id,
                    content,
                } => Some((message_id, content)),
                TurnInput::ManualCompression | TurnInput::ResetContext => None,
            })
            .collect();
        if user_messages.is_empty() {
            return;
        }
        // 任何失败（加载会话/单条保存/落盘）都记录到调度状态，由
        // shutdown_agent 在 join 后向调用方返回明确失败，并逐条发送错误
        // 事件——已接受消息不得静默丢失（ALR-202/206）。
        let mut failed: Vec<String> = Vec::new();
        match self.load_session() {
            Ok(mut session) => {
                for (message_id, content) in user_messages {
                    match session
                        .try_append_prepared_user_message_with_id(message_id.clone(), content)
                    {
                        Ok(()) => {
                            let _ = self.stream_tx.send(StreamEvent::Error {
                                message: format!(
                                    "会话已关闭，消息 {message_id} 已保存但未处理；重新打开会话后可继续"
                                ),
                            });
                        }
                        Err(error) => {
                            failed.push(format!("消息 {message_id} 保存失败：{error}"));
                            let _ = self.stream_tx.send(StreamEvent::Error {
                                message: format!("会话已关闭，消息 {message_id} 未能保存：{error}"),
                            });
                        }
                    }
                }
                if let Err(error) = session.try_persist_to_disk() {
                    failed.push(format!("关闭排空落盘失败：{error}"));
                }
            }
            Err(error) => {
                failed.push(format!("关闭排空加载会话失败：{error}"));
                let _ = self.stream_tx.send(StreamEvent::Error {
                    message: "会话已关闭，未处理消息未能持久化".to_string(),
                });
            }
        }
        if !failed.is_empty() {
            scheduling.set_close_error(failed.join("；"));
        }
    }
}

/// 唯一 driver 主循环：排空 Inbox、连续执行 turn、空闲挂起等待唤醒。
///
/// - 领取与唤醒判定同临界区（`try_park`），取消收敛窗口到达的消息不丢失（ALR-105）；
/// - 每个 turn 从最新 Session 构建（ALR-204），同一 driver 连续处理（ALR-104）；
/// - 关闭后不再执行新 turn，未处理输入持久化后退出（ALR-206）。
async fn drive_agent(scheduling: Arc<AgentScheduling>, spawner: TurnSpawner) {
    loop {
        if !scheduling.is_accepting() {
            spawner.persist_pending_on_shutdown(&scheduling);
            crate::react::inbox::remove_agent(&spawner.session_id);
            return;
        }
        let Some(input) = scheduling.take_input() else {
            if scheduling.try_park() {
                scheduling.wait_wake().await;
            }
            continue;
        };
        match input {
            TurnInput::UserMessage {
                message_id,
                content,
            } => {
                // next_step 只在真正开始用户 turn 时领取（ALR-103）；维护操作
                // （压缩/重置）不 drain，积压保留给下一次用户活动。
                let pending_steps = scheduling.take_next_steps();
                spawner
                    .run_user_turn(&scheduling, message_id, content, pending_steps)
                    .await;
            }
            TurnInput::ManualCompression => spawner.run_manual_compression(&scheduling).await,
            TurnInput::ResetContext => spawner.run_reset_context(),
        }
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

    /// 多个未启动活动的 Core 不创建 driver 任务，且可正常关闭。
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

        // 从未投递输入的 Core 不创建 driver（is_stopped 语义：未关闭即可接收；
        // 尚无任何活动时注册表中无条目）。
        for core in &cores {
            assert!(core.is_stopped(), "未启动活动的 Core 不应存在 driver");
        }

        // 逐个关闭，验证 shutdown 路径在共享 runtime 上正常工作
        for core in cores {
            let result = tokio::task::spawn_blocking(move || core.shutdown_join()).await;
            assert!(result.is_ok(), "shutdown_join 不应 panic");
        }
    }
}
