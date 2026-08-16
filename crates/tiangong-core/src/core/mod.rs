//! TiangongCore：天工智能体核心
//!
//! 输入经 [`AgentInput::deliver`] 进入：空闲起轮（Core 直接构建 TurnContext 并
//! spawn turn task）、运行中投通道（命令进入活跃 turn 的命令通道，由 turn 内
//! 部仲裁：引导/审批/取消）。每个 turn 从最新磁盘 Session 构建上下文（ALR-201/204）。
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
#[derive(TypedBuilder, Clone)]
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
    /// 测试专用的模型客户端；发布构建不存在该字段及 builder 配置入口。
    #[cfg(test)]
    #[builder(default, setter(strip_option))]
    test_client: Option<SingleProviderClient>,
}

impl TiangongCore {
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

    /// 是否有活跃的 turn task（有则视为 Running）。
    pub fn is_stopped(&self) -> bool {
        !crate::shared_runtime::is_running(&self.session_id)
    }

    /// 是否正在执行 turn（对外 `Running`）。
    pub fn is_busy(&self) -> bool {
        crate::shared_runtime::is_running(&self.session_id)
    }

    /// 设置会话信任模式。
    ///
    /// 更新后对当前会话的权限门与插件实时生效；活跃 turn 通过命令通道同步。
    pub fn set_trust_mode(&self, mode: crate::permission::TrustMode) {
        // Core 和插件立即使用新值；Session 字段由活跃 turn 或下一轮统一写入。
        *self.trust_mode.lock().unwrap_or_else(|p| p.into_inner()) = mode;
        for plugin in &self.plugins {
            plugin.set_trust_mode(mode);
        }
        if self.is_busy() {
            let _ =
                crate::shared_runtime::send_command(&self.session_id, Command::SetTrustMode(mode));
        }
    }

    /// 设置会话思考强度。
    ///
    /// 已经发出的模型请求不变；活跃 turn 会在下一次构建模型请求时使用新值。
    pub fn set_reasoning_effort(&self, effort: String) {
        self.config
            .update(|config| config.reasoning_effort = effort.clone());
        if self.is_busy() {
            let _ = crate::shared_runtime::send_command(
                &self.session_id,
                Command::SetReasoningEffort(effort),
            );
        }
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

    /// 更新会话标题。
    ///
    /// - turn 进行中：投 `Command::SetTitle`，由 turn 在命令分支写入 `ctx.session`，
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
        if self.is_busy() {
            return if crate::shared_runtime::send_command(
                &self.session_id,
                Command::SetTitle {
                    title,
                    only_if_default,
                },
            ) {
                Ok(())
            } else {
                Err(CoreError::WorkerStopped)
            };
        }
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
        #[cfg(test)]
        let (client, lite_client) = if let Some(test_client) = self.test_client.clone() {
            let test_client = test_client.with_on_retry(on_retry.clone());
            let lite_client = config.llm.lite.as_ref().map(|_| test_client.clone());
            (test_client, lite_client)
        } else {
            (
                SingleProviderClient::new(config.llm.chat.clone()).with_on_retry(on_retry.clone()),
                config.llm.lite.clone().map(SingleProviderClient::new),
            )
        };
        #[cfg(not(test))]
        let client =
            SingleProviderClient::new(config.llm.chat.clone()).with_on_retry(on_retry.clone());
        #[cfg(not(test))]
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

    /// 空闲起轮：构建上下文 → 校验并保存用户消息（成功才确认）→ spawn turn task。
    ///
    /// on_session_ready 仅在本 Core 实例的首次 turn 执行（会话级一次性初始化），
    /// 与系统提示重建、落盘一起在任务启动闭包内同步完成后再进入 Agent Loop。
    fn start_user_turn(
        &self,
        message_id: String,
        content: Vec<tiangong_types::ContentBlock>,
    ) -> Result<(), CoreError> {
        let mut ctx = self.build_turn_context()?;
        // 保存成功才确认（ALR-202）；失败向调用方返回明确错误，不虚报成功。
        ctx.session
            .try_append_prepared_user_message_with_id(message_id.clone(), content.clone())
            .map_err(|error| {
                tracing::warn!(%error, session_id = %self.session_id, "用户消息保存失败");
                CoreError::WorkerStopped
            })?;
        let _ = ctx.stream_tx.send(StreamEvent::UserMessage {
            message_id,
            content: tiangong_types::content_blocks_text(&content),
            content_blocks: tiangong_types::stable_content_blocks(&content),
            media: Vec::new(),
            model_excluded: false,
        });
        let session_ready = self.session_ready.clone();
        let core = self.clone();
        crate::shared_runtime::spawn_turn(ctx, move |mut ctx, cmd_rx| {
            if session_ready
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
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
            if let Err(error) = ctx.session.try_persist_to_disk() {
                tracing::warn!(%error, "持久化本轮系统提示失败");
            }
            let mut cmd_rx = cmd_rx;
            Ok(async move {
                run_turn(ctx, &mut cmd_rx).await;
                // 收尾排空：turn 已定终态，通道中未被消费的用户消息不丢弃。
                // 与引导语义一致——连发消息以最后一条为准：其余消息保存进会话
                // 作为历史（不丢），最后一条直接续轮（start_user_turn 自带保存
                // 与确认，不再重复落盘）。关闭（Cancel）路径同样经此保存。
                let mut pending_user_messages = Vec::new();
                while let Ok(command) = cmd_rx.try_recv() {
                    if let Command::InjectUserMessage {
                        message_id,
                        content,
                    } = command
                    {
                        pending_user_messages.push((message_id, content));
                    }
                }
                if let Some((message_id, content)) = pending_user_messages.pop() {
                    if !pending_user_messages.is_empty() {
                        core.save_pending_user_messages(&pending_user_messages);
                    }
                    crate::shared_runtime::release_agent(&core.session_id);
                    if let Err(error) = core.start_user_turn(message_id, content) {
                        tracing::warn!(%error, session_id = %core.session_id, "接续排队消息起轮失败");
                    }
                }
            })
        })
    }

    /// 把未被消费的用户消息保存进会话并落盘（不丢）。
    fn save_pending_user_messages(&self, messages: &[(String, Vec<tiangong_types::ContentBlock>)]) {
        let Ok(mut session) = self.load_session() else {
            tracing::warn!(session_id = %self.session_id, "排队消息落盘前加载会话失败");
            return;
        };
        for (message_id, content) in messages {
            if let Err(error) = session
                .try_append_prepared_user_message_with_id(message_id.clone(), content.clone())
            {
                tracing::warn!(%error, message_id, "排队消息保存失败");
            }
        }
        if let Err(error) = session.try_persist_to_disk() {
            tracing::warn!(%error, session_id = %self.session_id, "排队消息落盘失败");
        }
    }

    /// 活动期输入守门：仅当有活跃 turn 时投递其命令通道；空闲时明确拒绝。
    fn deliver_to_turn(
        &self,
        command: Command,
        message_type: &'static str,
    ) -> Result<(), CoreError> {
        if !self.is_busy() {
            // 空闲工具注入不丢弃：写入会话延迟队列，下一轮模型请求自动携带。
            if let Command::InjectTool { tool_name, payload } = command {
                let mut session = self.load_session()?;
                session.defer_tool_injection(tool_name, payload);
                session.try_persist_to_disk().map_err(|error| {
                    tracing::warn!(%error, session_id = %self.session_id, "空闲注入落盘失败");
                    CoreError::WorkerStopped
                })?;
                return Ok(());
            }
            tracing::warn!(session_id = %self.session_id, message_type, "Agent 空闲，拒绝活动期输入");
            return Err(CoreError::WorkerStopped);
        }
        if crate::shared_runtime::send_command(&self.session_id, command) {
            Ok(())
        } else {
            Err(CoreError::WorkerStopped)
        }
    }

    /// 空闲期手动上下文压缩：spawn 独立压缩任务占用本会话的任务槽。
    ///
    /// 压缩期间收到用户消息会取消压缩并直接起轮（压缩可随时重新发起）；
    /// 其他中断类命令按各自语义处理。运行中调用返回 `Busy`。
    fn compress_context(&self) -> Result<(), CoreError> {
        if self.is_busy() {
            return Err(CoreError::Busy);
        }
        let ctx = self.build_turn_context()?;
        let core = self.clone();
        let session_id = self.session_id.clone();
        crate::shared_runtime::spawn_turn(ctx, move |mut ctx, mut cmd_rx| {
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
            Ok(async move {
                if let Some(crate::react::compression::CompressionInterrupt::Command(command)) =
                    crate::react::compression::run_manual_context_compression(ctx, &mut cmd_rx)
                        .await
                {
                    // 压缩已被该命令取消且未应用任何结果。腾出任务槽后再按命令
                    // 类型接续：用户消息起新轮，标题就地落盘，其余记录后丢弃。
                    crate::shared_runtime::release_agent(&session_id);
                    match command {
                        Command::InjectUserMessage {
                            message_id,
                            content,
                        } => {
                            if let Err(error) = core.start_user_turn(message_id, content) {
                                tracing::warn!(%error, session_id = %session_id, "压缩中断后起新轮失败");
                            }
                        }
                        Command::SetTitle {
                            title,
                            only_if_default,
                        } => {
                            let _ = core.set_title(title, only_if_default);
                        }
                        Command::SetReasoningEffort(effort) => {
                            core.set_reasoning_effort(effort);
                        }
                        Command::SetTrustMode(mode) => {
                            core.set_trust_mode(mode);
                        }
                        Command::Cancel | Command::Shutdown => {}
                        command => {
                            tracing::warn!(
                                session_id = %session_id,
                                message_type = command.kind_name(),
                                "手动压缩被中断，命令已丢弃"
                            );
                        }
                    }
                }
            })
        })
    }

    /// 空闲期清理上下文（同步执行，不涉及模型请求）。
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
        // 取消活跃 turn 并等待任务收敛，再投递插件收尾通知、返回最终 session。
        crate::shared_runtime::cancel_and_join(&self.session_id)?;
        let session = self.load_session()?;
        self.finalize_plugins(&session);
        Ok(session)
    }

    /// 投递全部 `on_session_ended` 通知（worker 退出前的 finalize 钩子）。
    ///
    /// 通知型钩子：后台线程投递、不等待完成——关闭会话/退出应用不被任何插件
    /// 无限阻塞（issue #404）。钩子收到只读快照，收尾成败与产出由插件自行负责。
    fn finalize_plugins(&self, session: &Session) {
        crate::core::plugin::notify_session_ended(&self.plugins, session);
    }
}

impl crate::agent_input::AgentInput for TiangongCore {
    fn deliver(&self, input: crate::agent_input::AgentInputKind) -> Result<(), CoreError> {
        use crate::agent_input::{AgentInputKind, ApprovalInput, CommandInput, MessageInput};

        match input {
            // 空闲起轮、运行中投通道：运行中的用户消息由活跃 turn 作为引导处理
            // （中断当前活动 → 保存新消息 → 从新意图重启）。
            AgentInputKind::Message(MessageInput::UserMessage {
                prepared,
                message_id,
            }) => {
                let message_id = message_id.unwrap_or_else(scru128::new_string);
                if self.is_busy() {
                    return if crate::shared_runtime::send_command(
                        &self.session_id,
                        Command::InjectUserMessage {
                            message_id,
                            content: prepared,
                        },
                    ) {
                        Ok(())
                    } else {
                        Err(CoreError::WorkerStopped)
                    };
                }
                self.start_user_turn(message_id, prepared)
            }
            AgentInputKind::Tool(tool) => self.deliver_to_turn(
                Command::InjectTool {
                    tool_name: tool.tool_name().to_string(),
                    payload: tool.render(),
                },
                "InjectTool",
            ),
            AgentInputKind::Approval(ApprovalInput::Response {
                request_id,
                approved,
            }) => self.deliver_to_turn(
                Command::Approval {
                    request_id,
                    approved,
                },
                "ApprovalResponse",
            ),
            AgentInputKind::Command(cmd) => match cmd {
                CommandInput::Cancel => {
                    let _ = crate::shared_runtime::send_command(&self.session_id, Command::Cancel);
                    Ok(())
                }
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
        // 空实现：turn task 在共享 runtime 上自然跑完并落盘（含 agent-team 子
        // Agent 被父轮释放的场景）。显式取消与关闭由 Cancel 命令、shutdown_join
        // / CoreManager 承担；不在此发 Cancel，避免任务闭包持有的 Core 克隆在
        // 释放时误杀自己启动的后继任务。
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
