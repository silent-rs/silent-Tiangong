use std::collections::{HashMap, VecDeque, hash_map::Entry};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
#[cfg(test)]
use std::time::Duration;

use crate::api::SharedState;
use crate::remote::event::{EventBus, TiangongEvent};
use anyhow::{Result, anyhow};
use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
use tiangong_core::permission::TrustMode;
use tiangong_core::session::{Message, MessageRole, MessageToolCall, Session, now_text};
use tiangong_media_archive::{
    AttachmentCapabilitySnapshot, AttachmentStore, AttachmentTransaction, RawAttachment,
};
use tiangong_types::{
    MediaAsset, MediaKind, MessageContent, OutgoingMessage, PreparedUserMessage,
    SessionStreamEvent, StreamEvent,
};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Clone)]
pub struct ServerCoreManager {
    state: SharedState,
    config: CoreConfigProvider,
    event_bus: Arc<EventBus>,
    /// MCP 管理插件共享句柄：core 注册与 API 管理使用同一实例（dual-ownership），
    /// 避免 API 修改的配置与运行中 core 的 plugin 状态分叉。
    mcp_plugin: Arc<tiangong_plugin_mcp::McpPlugin>,
    cores: Arc<Mutex<HashMap<String, tiangong_core::core::TiangongCore>>>,
    /// 同一会话的 Core 创建、消息投递和删除共用这一把锁。
    ///
    /// 锁对象不主动删除，避免旧等待者尚未退出时为同一 session 创建第二把锁。
    session_operation_locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    session_wait_locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    /// 串行全局配置刷新与新 Core 安装，避免新实例错过刚完成的配置更新。
    config_update_lock: Arc<AsyncMutex<()>>,
    core_attachment_capabilities: Arc<Mutex<HashMap<String, AttachmentCapabilitySnapshot>>>,
    trackers: Arc<Mutex<HashMap<String, Arc<ExecutionTracker>>>>,
    remote_sessions: Arc<Mutex<HashMap<String, String>>>,
}

impl ServerCoreManager {
    pub fn new(
        state: SharedState,
        config: CoreConfigProvider,
        event_bus: Arc<EventBus>,
        mcp_plugin: Arc<tiangong_plugin_mcp::McpPlugin>,
    ) -> Self {
        Self {
            state,
            config,
            event_bus,
            mcp_plugin,
            cores: Arc::new(Mutex::new(HashMap::new())),
            session_operation_locks: Arc::new(Mutex::new(HashMap::new())),
            session_wait_locks: Arc::new(Mutex::new(HashMap::new())),
            config_update_lock: Arc::new(AsyncMutex::new(())),
            core_attachment_capabilities: Arc::new(Mutex::new(HashMap::new())),
            trackers: Arc::new(Mutex::new(HashMap::new())),
            remote_sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 从 App State 刷新全局模板，并按 session 为每个 Core 替换独立配置快照。
    pub async fn sync_config_from_state(&self) {
        let _config_guard = self.config_update_lock.lock().await;
        let base = self.config.snapshot();
        let (template, session_configs) = {
            let state = self.state.lock().await;
            tiangong_config::registry::set_models(state.models_config().clone());

            let mut template = state.build_core_config_for_session_from_base(&base, "");
            template.trust_mode = TrustMode::FullTrust;
            let session_configs = state
                .sessions()
                .iter()
                .map(|session| {
                    let mut config =
                        state.build_core_config_for_session_from_base(&base, &session.id);
                    config.trust_mode = TrustMode::FullTrust;
                    (session.id.clone(), config)
                })
                .collect::<HashMap<_, _>>();
            (template, session_configs)
        };

        let cores = match self.cores.lock() {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(error = %error, "会话 Core 锁已损坏，恢复后更新配置");
                error.into_inner()
            }
        };
        self.config.replace(template);
        for (session_id, core) in cores.iter() {
            if let Some(config) = session_configs.get(session_id)
                && let Err(error) = core.replace_config(config.clone())
            {
                tracing::warn!(session_id, error = %error, "会话 Core 配置更新失败");
            }
        }
    }

    fn session_operation_lock(&self, session_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = match self.session_operation_locks.lock() {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(error = %error, "会话操作锁表已损坏，恢复后继续");
                error.into_inner()
            }
        };
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn session_wait_lock(&self, session_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = match self.session_wait_locks.lock() {
            Ok(locks) => locks,
            Err(poisoned) => poisoned.into_inner(),
        };
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    pub async fn send_connector_message_and_wait(
        &self,
        connector: &str,
        channel_id: &str,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
    ) -> Result<(String, OutgoingMessage)> {
        let session_id = self
            .resolve_connector_session_id(connector, channel_id)
            .await?;
        self.send_message_and_wait(&session_id, content, message_id, media)
            .await
    }

    /// 发送消息到 Core。所有入口都串行到当前轮次真正结束，避免后台消息在等待型
    /// 请求执行期间被吸收到同一轮、导致回复归属错乱。
    pub async fn send_message(
        &self,
        requested_session_id: &str,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
    ) -> Result<()> {
        let _ = self
            .send_message_and_wait(requested_session_id, content, message_id, media)
            .await?;
        Ok(())
    }

    pub async fn send_message_and_wait(
        &self,
        requested_session_id: &str,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
    ) -> Result<(String, OutgoingMessage)> {
        let requested_session_id = normalize_session_id(requested_session_id)?;
        // 一个 Core 执行可吸收运行中追加消息但只产生一个终态；等待型入口按会话
        // 串行到终态，确保每个调用拿到与自身消息对应的回复。DELETE 使用独立锁，
        // 仍可随时取消正在等待的 Core。
        let wait_lock = self.session_wait_lock(&requested_session_id);
        let _wait_guard = wait_lock.lock().await;
        let session_lock = self.session_operation_lock(&requested_session_id);
        let session_guard = session_lock.lock().await;
        let (session_id, capabilities) = self.ensure_core_locked(&requested_session_id).await?;

        let msg_id = message_id.unwrap_or_else(|| scru128::new().to_string());
        // 附件准备成功后才登记 waiter，准备失败不会污染 tracker。
        let (transaction, prepared) = self
            .prepare_user_message(msg_id.clone(), content, media, capabilities)
            .await?;
        let tracker = self.tracker_for(&session_id);
        let turn_id = tracker.start_turn(msg_id.clone());
        let receipt = match self.enqueue_prepared(&session_id, &msg_id, prepared) {
            Ok(receipt) => receipt,
            Err(error) => {
                tracker.cancel_turn(turn_id);
                return Err(error);
            }
        };
        let created_paths = commit_enqueued_attachment_transaction(transaction);
        if let Err(error) = receipt.await_persisted().await {
            tracker.cancel_turn(turn_id);
            return Err(attachment_delivery_failure(error, &created_paths));
        }
        // 消息稳定内容已确认；完整 turn 可能持续较久，释放锁让 DELETE 可以取消 Core。
        drop(session_guard);

        let tracker_for_wait = tracker.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let outcome = tracker_for_wait.wait_for_turn(turn_id);
            let _ = tx.send(outcome);
        });
        let outcome = match rx.await {
            Ok(outcome) => outcome,
            Err(_) => {
                tracker.cancel_turn(turn_id);
                return Err(anyhow!("等待执行结果的线程意外退出"));
            }
        };
        let response = match outcome {
            TurnOutcome::Completed => self.last_assistant_outgoing(&session_id).await.0,
            TurnOutcome::Failed(message) => {
                let (response, is_direct_agent_reply) =
                    self.last_assistant_outgoing(&session_id).await;
                if is_direct_agent_reply {
                    // @Agent 的业务失败/定向取消已经生成可交付说明；会话轮次仍记录为
                    // failed/cancelled，但远程用户不能因为终态是 Error 而看不到说明。
                    response
                } else {
                    return Err(anyhow!(message));
                }
            }
        };

        Ok((session_id, response))
    }

    /// 停止并等待目标 Core 退出后，再删除会话状态和文件。
    ///
    /// 返回 `false` 表示会话本来就不存在；即使如此也会清理同 ID 的孤立 Core。
    pub async fn delete_session(&self, requested_session_id: &str) -> Result<bool> {
        let session_id = normalize_session_id(requested_session_id)?;
        let session_lock = self.session_operation_lock(&session_id);
        let _session_guard = session_lock.lock().await;

        let core = self
            .cores
            .lock()
            .map_err(|error| anyhow!("会话 Core 锁已损坏：{error}"))?
            .remove(&session_id);
        if let Some(core) = core {
            if let Err(error) = core.deliver(AgentInputKind::cancel()) {
                tracing::warn!(session_id, error = %error, "删除会话前触发 Core 取消失败");
            }
            let joined_session_id = session_id.clone();
            match tokio::task::spawn_blocking(move || core.into_session()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    // worker 已完成 join；即使它以 panic 结束，也不再有后台写入者。
                    tracing::warn!(
                        session_id = joined_session_id,
                        error = %error,
                        "删除会话前关闭 Core 失败"
                    );
                }
                Err(error) => {
                    return Err(anyhow!("删除会话前等待 Core 关闭失败：{error}"));
                }
            }
        }

        self.core_attachment_capabilities
            .lock()
            .map_err(|error| anyhow!("附件能力快照锁已损坏：{error}"))?
            .remove(&session_id);
        self.trackers
            .lock()
            .map_err(|error| anyhow!("执行跟踪锁已损坏：{error}"))?
            .remove(&session_id);
        self.remote_sessions
            .lock()
            .map_err(|error| anyhow!("远程会话映射锁已损坏：{error}"))?
            .retain(|_, mapped_session_id| mapped_session_id != &session_id);

        let mut state = self.state.lock().await;
        if !state
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return Ok(false);
        }
        state.delete_session_by_id(&session_id)?;
        Ok(true)
    }

    async fn prepare_user_message(
        &self,
        message_id: String,
        content: String,
        media: Vec<MediaAsset>,
        capabilities: AttachmentCapabilitySnapshot,
    ) -> Result<(AttachmentTransaction, PreparedUserMessage)> {
        let raw = media.into_iter().map(raw_attachment_from_media).collect();
        let media_root = tiangong_app_state::app_state::storage_root().join("media");
        tokio::task::spawn_blocking(move || {
            prepare_user_message_blocking(media_root, raw, message_id, content, capabilities)
        })
        .await
        .map_err(|error| anyhow!("附件准备任务失败：{error}"))?
        .map_err(|error| anyhow!("附件准备失败：{error}"))
    }

    fn enqueue_prepared(
        &self,
        session_id: &str,
        message_id: &str,
        prepared: PreparedUserMessage,
    ) -> Result<tiangong_core::core::PreparedMessageReceipt> {
        let cores = self
            .cores
            .lock()
            .map_err(|error| anyhow!("会话 Core 锁已损坏：{error}"))?;
        let core = cores
            .get(session_id)
            .ok_or_else(|| anyhow!("会话 core 不存在：{session_id}"))?;
        core.enqueue_prepared_with_receipt(message_id.to_string(), prepared)
            .map_err(|error| anyhow!("消息投递失败：{error}"))
    }

    /// 调用方必须持有目标 session 的 `session_operation_lock`。
    async fn ensure_core_locked(
        &self,
        requested_session_id: &str,
    ) -> Result<(String, AttachmentCapabilitySnapshot)> {
        let session_id = normalize_session_id(requested_session_id)?;
        {
            let state = self.state.lock().await;
            resolve_explicit_session(state.sessions(), &session_id)?;
        }

        // 先检查是否已有可用 Core；事件流已关闭的僵尸实例必须先移除再重建。
        let has_live_core = {
            let mut cores = self.cores.lock().unwrap();
            match cores.get(&session_id) {
                Some(core) if !core.is_stopped() => true,
                Some(_) => {
                    cores.remove(&session_id);
                    self.core_attachment_capabilities
                        .lock()
                        .unwrap()
                        .remove(&session_id);
                    false
                }
                None => false,
            }
        };
        if has_live_core {
            let capabilities = self
                .core_attachment_capabilities
                .lock()
                .unwrap()
                .get(&session_id)
                .copied()
                .ok_or_else(|| anyhow!("会话 Core 缺少附件能力快照：{session_id}"))?;
            return Ok((session_id, capabilities));
        }

        let (stream_tx, stream_rx) = mpsc::channel::<SessionStreamEvent>();

        // 初始化 Memory Handle（入口层负责，构造时注入 memory 插件）。
        let memory_handle = tiangong_memory::registry::init_memory_handle_for_process(
            self.config.generation(),
            tiangong_memory::ProcessType::Server,
        )
        .await;

        let _config_guard = self.config_update_lock.lock().await;

        // async 初始化期间仍做第二次检查。会话锁保证正常路径不会并发创建，
        // 这里同时防御未来新增的未加锁入口。
        let has_live_core = {
            let mut cores = self.cores.lock().unwrap();
            match cores.get(&session_id) {
                Some(core) if !core.is_stopped() => true,
                Some(_) => {
                    cores.remove(&session_id);
                    self.core_attachment_capabilities
                        .lock()
                        .unwrap()
                        .remove(&session_id);
                    false
                }
                None => false,
            }
        };
        if has_live_core {
            let capabilities = self
                .core_attachment_capabilities
                .lock()
                .unwrap()
                .get(&session_id)
                .copied()
                .ok_or_else(|| anyhow!("会话 Core 缺少附件能力快照：{session_id}"))?;
            return Ok((session_id, capabilities));
        }

        // 删除或其他状态变化若绕过了会话锁，禁止使用 await 前的旧快照复活 Core。
        let base = self.config.snapshot();
        let (session, mut session_config) = {
            let state = self.state.lock().await;
            let session = resolve_explicit_session(state.sessions(), &session_id)?.1;
            let config = state.build_core_config_for_session_from_base(&base, &session_id);
            (session, config)
        };
        session_config.trust_mode = TrustMode::FullTrust;

        let models = tiangong_config::registry::models();
        let attachment_capabilities = attachment_capability_snapshot(&models);
        let core = tiangong_core::core::TiangongCore::builder()
            .config(isolated_core_config_provider(&session_config))
            .session(session.clone())
            .event_sender(stream_tx)
            .plugins({
                // app 层判断是否注册各能力插件，经 llm 路由解析端点后构造注入。
                // 与 attachment_capabilities 使用同一份 models 快照，保证 Planner
                // 看到的能力与该 Core 实际注册插件一致。
                use tiangong_llm::{ModelCapability, ModelEndpoint, SingleProviderClient};
                let resolve_ep = |cap: ModelCapability| {
                    models
                        .resolve_for_capability(cap)
                        .map(ModelEndpoint::from_resolved)
                };
                let mut plugins = tiangong_plugin_fs::default_plugins();
                plugins.extend(tiangong_plugin_index::default_plugins());
                if let Some(ep) = resolve_ep(ModelCapability::ImageGeneration) {
                    plugins.push(tiangong_plugin_generate_image::build_plugin(ep));
                }
                if let Some(ep) = resolve_ep(ModelCapability::VideoGeneration) {
                    plugins.push(tiangong_plugin_generate_video::build_plugin(ep));
                }
                if let Some(ep) = resolve_ep(ModelCapability::Tts) {
                    plugins.push(tiangong_plugin_text_to_speech::build_plugin(ep));
                }
                if let Some(ep) = resolve_ep(ModelCapability::Stt) {
                    plugins.push(tiangong_plugin_speech_to_text::build_plugin(ep));
                }
                plugins.extend(tiangong_plugin_memory::default_plugins(memory_handle));
                plugins.extend(tiangong_plugin_fetch::default_plugins());
                plugins.extend(tiangong_plugin_command::default_plugins());
                plugins.extend(tiangong_plugin_scheduler::default_plugins());
                plugins.extend(tiangong_plugin_task::default_plugins());
                if models.has_capability(ModelCapability::Multimodal)
                    && !models.chat_is_multimodal()
                    && let Some(client) =
                        resolve_ep(ModelCapability::Multimodal).map(SingleProviderClient::new)
                {
                    plugins.push(tiangong_plugin_analyze_attachment::build_plugin(client));
                }
                // Skill 详情查询（get_skill_detail）：无条件注册，插件内部按是否存在
                // 已启用 skill 决定是否暴露工具与注入 prompt 段落。
                plugins.extend(tiangong_plugin_skill::default_plugins());
                // MCP 工具（动态收集 MCP server 工具 + 执行分发）：
                // 共享 ServerAppContext 持有的同一 plugin 实例，确保 API 管理操作
                //（register/remove/set_enabled）与运行中 core 的 plugin 状态一致。
                plugins.push(self.mcp_plugin.clone());
                plugins
            })
            .storage(tiangong_core::core::CoreStorageLocation::new(
                tiangong_app_state::app_state::storage_root(),
            ))
            .build()?;
        core.set_trust_mode(TrustMode::FullTrust);
        let actual_session_id = core.session_id().to_string();
        let installed =
            self.install_core_if_absent(&actual_session_id, core, attachment_capabilities)?;

        if !installed {
            let capabilities = self
                .core_attachment_capabilities
                .lock()
                .unwrap()
                .get(&actual_session_id)
                .copied()
                .ok_or_else(|| anyhow!("会话 Core 缺少附件能力快照：{actual_session_id}"))?;
            return Ok((actual_session_id, capabilities));
        }

        let tracker = self.tracker_for(&actual_session_id);
        self.spawn_stream_forwarder(actual_session_id.clone(), stream_rx, tracker);

        Ok((actual_session_id, attachment_capabilities))
    }

    fn install_core_if_absent(
        &self,
        session_id: &str,
        core: tiangong_core::core::TiangongCore,
        attachment_capabilities: AttachmentCapabilitySnapshot,
    ) -> Result<bool> {
        let mut cores = self
            .cores
            .lock()
            .map_err(|error| anyhow!("会话 Core 锁已损坏：{error}"))?;
        match cores.entry(session_id.to_string()) {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(entry) => {
                self.core_attachment_capabilities
                    .lock()
                    .map_err(|error| anyhow!("附件能力快照锁已损坏：{error}"))?
                    .insert(session_id.to_string(), attachment_capabilities);
                entry.insert(core);
                Ok(true)
            }
        }
    }

    async fn resolve_connector_session_id(
        &self,
        connector: &str,
        channel_id: &str,
    ) -> Result<String> {
        let channel_id = channel_id.trim();
        if channel_id.is_empty() {
            let state = self.state.lock().await;
            return Ok(state.active_session_id().to_string());
        }

        let key = remote_session_key(connector, channel_id);
        if let Some(session_id) = self.remote_sessions.lock().unwrap().get(&key).cloned() {
            return Ok(session_id);
        }

        let title = remote_session_title(connector, channel_id);
        let session_id = {
            let mut state = self.state.lock().await;
            if state
                .sessions()
                .iter()
                .any(|session| session.id == channel_id)
            {
                channel_id.to_string()
            } else if let Some(session) = state
                .sessions()
                .iter()
                .find(|session| session.title == title)
            {
                session.id.clone()
            } else {
                let mut session = Session::new_isolated(title);
                session.trust_mode = TrustMode::FullTrust;
                let session_id = session.id.clone();
                state.sessions_mut().push(session);
                state.persist_session(&session_id)?;
                self.event_bus
                    .publish(TiangongEvent::SessionCreated(session_id.clone()));
                session_id
            }
        };

        self.remote_sessions
            .lock()
            .unwrap()
            .insert(key, session_id.clone());
        Ok(session_id)
    }

    fn spawn_stream_forwarder(
        &self,
        session_id: String,
        stream_rx: mpsc::Receiver<SessionStreamEvent>,
        tracker: Arc<ExecutionTracker>,
    ) {
        let state = self.state.clone();
        let event_bus = self.event_bus.clone();
        thread::spawn(move || {
            for session_event in stream_rx {
                let event = session_event.event;
                sync_stream_event_to_state(&state, &event_bus, &session_id, &event);
                // 终态先完成 Core 权威会话重载，再唤醒等待者；否则等待线程可能读到
                // 尚未提交的流式镜像。
                tracker.observe_event(&event);
            }
            tracker.fail_all("Core 事件流已关闭");
        });
    }

    fn tracker_for(&self, session_id: &str) -> Arc<ExecutionTracker> {
        let mut trackers = self.trackers.lock().unwrap();
        trackers
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(ExecutionTracker::default()))
            .clone()
    }

    async fn last_assistant_outgoing(&self, session_id: &str) -> (OutgoingMessage, bool) {
        let state = self.state.lock().await;
        let Some(session) = state
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
        else {
            return (text_outgoing("处理完成"), false);
        };

        assistant_outgoing_after_last_user(session)
    }
}

fn assistant_outgoing_after_last_user(session: &Session) -> (OutgoingMessage, bool) {
    let last_user_index = session
        .messages
        .iter()
        .rposition(|message| message.role == MessageRole::User)
        .unwrap_or(0);
    let assistant_messages = session
        .messages
        .iter()
        .skip(last_user_index + 1)
        .filter(|message| message.role == MessageRole::Assistant)
        .collect::<Vec<_>>();
    let has_direct_agent_reply = assistant_messages.iter().any(|message| {
        message.model_excluded
            && message
                .worker_id
                .as_deref()
                .is_some_and(|worker_id| worker_id.starts_with("agent:"))
    });
    let selected = assistant_messages.into_iter().filter(|message| {
        !has_direct_agent_reply
            || (message.model_excluded
                && message
                    .worker_id
                    .as_deref()
                    .is_some_and(|worker_id| worker_id.starts_with("agent:")))
    });
    let mut texts = Vec::new();
    let mut media_items = Vec::new();
    for message in selected {
        let text = message.text_content();
        if !text.trim().is_empty() {
            if has_direct_agent_reply {
                texts.push(text);
            } else {
                texts.clear();
                texts.push(text);
            }
        }
        for block in &message.content {
            match block {
                tiangong_types::ContentBlock::Media {
                    kind,
                    url,
                    mime_type,
                    title,
                } => {
                    media_items.push(MediaAsset {
                        kind: *kind,
                        url: url.clone(),
                        mime_type: mime_type.clone(),
                        title: title.clone(),
                        capability: None,
                    });
                }
                tiangong_types::ContentBlock::Image { asset, .. }
                | tiangong_types::ContentBlock::AssetReference { asset } => {
                    media_items.push(MediaAsset {
                        kind: asset.kind,
                        url: asset.local_path.clone(),
                        mime_type: Some(asset.mime_type.clone()),
                        title: Some(asset.original_name.clone()),
                        capability: None,
                    });
                }
                tiangong_types::ContentBlock::Text { .. }
                | tiangong_types::ContentBlock::ModelInstruction { .. } => {}
            }
        }
    }
    let latest_text = texts.join("\n\n");

    if !media_items.is_empty() {
        return (
            media_outgoing(media_items, latest_text),
            has_direct_agent_reply,
        );
    }

    if latest_text.trim().is_empty() {
        (text_outgoing("处理完成"), has_direct_agent_reply)
    } else {
        (text_outgoing(latest_text), has_direct_agent_reply)
    }
}

fn raw_attachment_from_media(asset: MediaAsset) -> RawAttachment {
    RawAttachment {
        kind: asset.kind,
        source: asset.url,
        mime_type: asset.mime_type,
        original_name: asset.title,
    }
}

fn commit_enqueued_attachment_transaction(
    transaction: AttachmentTransaction,
) -> Vec<std::path::PathBuf> {
    let created_paths = transaction.newly_created_paths().to_vec();
    transaction.commit();
    created_paths
}

fn attachment_delivery_failure(
    error: impl std::fmt::Display,
    created_paths: &[std::path::PathBuf],
) -> anyhow::Error {
    match cleanup_created_attachment_paths(created_paths) {
        Ok(()) => anyhow!("消息持久化失败：{error}"),
        Err(cleanup_error) => anyhow!("消息持久化失败：{error}；清理本批附件失败：{cleanup_error}"),
    }
}

fn cleanup_created_attachment_paths(paths: &[std::path::PathBuf]) -> Result<()> {
    let mut failures = Vec::new();
    for path in paths {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(failures.join("；")))
    }
}

fn normalize_session_id(session_id: &str) -> Result<String> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Err(anyhow!("会话 ID 不能为空"));
    }
    Ok(session_id.to_string())
}

fn isolated_core_config_provider(config: &CoreConfig) -> CoreConfigProvider {
    CoreConfigProvider::new(config.clone())
}

fn resolve_explicit_session(
    sessions: &[Session],
    requested_session_id: &str,
) -> Result<(String, Session)> {
    let session_id = requested_session_id.trim();
    if session_id.is_empty() {
        return Err(anyhow!("会话 ID 不能为空"));
    }
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .cloned()
        .ok_or_else(|| anyhow!("会话不存在：{session_id}"))?;
    Ok((session_id.to_string(), session))
}

fn prepare_user_message_blocking(
    media_root: std::path::PathBuf,
    raw: Vec<RawAttachment>,
    message_id: String,
    content: String,
    capabilities: AttachmentCapabilitySnapshot,
) -> std::result::Result<(AttachmentTransaction, PreparedUserMessage), String> {
    let mut transaction = AttachmentStore::new(media_root).store_batch(raw)?;
    let prepared = transaction.prepare_message(&message_id, content, capabilities)?;
    Ok((transaction, prepared))
}

fn attachment_capability_snapshot(
    models: &tiangong_llm::ModelsConfig,
) -> AttachmentCapabilitySnapshot {
    use tiangong_llm::ModelCapability;

    let chat_multimodal = models.chat_is_multimodal();
    let analyze_attachment = !chat_multimodal
        && models.has_capability(ModelCapability::Multimodal)
        && models
            .resolve_for_capability(ModelCapability::Multimodal)
            .is_some();
    let audio_processor = models.has_capability(ModelCapability::Stt)
        && models
            .resolve_for_capability(ModelCapability::Stt)
            .is_some();

    AttachmentCapabilitySnapshot {
        chat_multimodal,
        analyze_attachment,
        audio_processor,
        video_processor: false,
    }
}

fn remote_session_key(connector: &str, channel_id: &str) -> String {
    format!("{}:{}", connector.trim(), channel_id.trim())
}

fn remote_session_title(connector: &str, channel_id: &str) -> String {
    let connector = connector.trim();
    let channel_id = channel_id.trim();
    let raw = if connector.is_empty() {
        format!("外部通道 {channel_id}")
    } else {
        format!("{connector} {channel_id}")
    };
    raw.chars().take(80).collect()
}

fn text_outgoing(text: impl Into<String>) -> OutgoingMessage {
    OutgoingMessage {
        content: MessageContent::Text(text.into()),
        attachments: Vec::new(),
        reply_to: None,
    }
}

fn media_outgoing(media: Vec<MediaAsset>, caption: String) -> OutgoingMessage {
    let mut media = media.into_iter();
    let first = media.next().expect("media_outgoing 至少需要一个媒体项");
    let content = media_content(first, (!caption.trim().is_empty()).then_some(caption));
    let attachments = media
        .map(|media| media_content(media, None))
        .collect::<Vec<_>>();
    OutgoingMessage {
        content,
        attachments,
        reply_to: None,
    }
}

fn media_content(media: MediaAsset, caption: Option<String>) -> MessageContent {
    match media.kind {
        MediaKind::Image => MessageContent::Image {
            url: media.url,
            caption,
        },
        MediaKind::Video => MessageContent::Video {
            url: media.url,
            caption,
        },
        MediaKind::Audio => MessageContent::Audio {
            url: media.url,
            duration: None,
        },
        MediaKind::File => MessageContent::File {
            name: media.title.unwrap_or_else(|| "文件".to_string()),
            url: media.url,
        },
    }
}

#[derive(Debug, Clone)]
enum TurnOutcome {
    Completed,
    Failed(String),
}

#[derive(Default)]
struct ExecutionTracker {
    state: Mutex<ExecutionTrackerState>,
    notify: Condvar,
}

#[derive(Default)]
struct ExecutionTrackerState {
    next_turn_id: u64,
    pending_by_message: HashMap<String, u64>,
    active_turns: VecDeque<u64>,
    outcomes: HashMap<u64, TurnOutcome>,
}

impl ExecutionTracker {
    fn start_turn(&self, message_id: String) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.next_turn_id += 1;
        let turn_id = state.next_turn_id;
        state.pending_by_message.insert(message_id, turn_id);
        state.outcomes.remove(&turn_id);
        turn_id
    }

    fn cancel_turn(&self, turn_id: u64) {
        let mut state = self.state.lock().unwrap();
        remove_turn_state(&mut state, turn_id);
        self.notify.notify_all();
    }

    fn observe_event(&self, event: &StreamEvent) {
        let mut state = self.state.lock().unwrap();

        match event {
            StreamEvent::UserMessage { message_id, .. } => {
                if let Some(turn_id) = state.pending_by_message.remove(message_id) {
                    state.active_turns.push_back(turn_id);
                }
            }
            StreamEvent::Done { .. } => {
                if let Some(turn_id) = state.active_turns.pop_front() {
                    state.outcomes.insert(turn_id, TurnOutcome::Completed);
                    self.notify.notify_all();
                }
            }
            StreamEvent::Error { message } => {
                if let Some(turn_id) = state.active_turns.pop_front() {
                    state
                        .outcomes
                        .insert(turn_id, TurnOutcome::Failed(message.clone()));
                    self.notify.notify_all();
                }
            }
            _ => {}
        }
    }

    fn wait_for_turn(&self, turn_id: u64) -> TurnOutcome {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(outcome) = state.outcomes.remove(&turn_id) {
                return outcome;
            }
            match self.notify.wait(state) {
                Ok(guard) => state = guard,
                Err(poisoned) => {
                    state = poisoned.into_inner();
                }
            }
        }
    }

    fn fail_all(&self, message: &str) {
        let mut state = self.state.lock().unwrap();
        let mut turn_ids = state
            .pending_by_message
            .values()
            .copied()
            .collect::<Vec<_>>();
        turn_ids.extend(state.active_turns.iter().copied());
        state.pending_by_message.clear();
        state.active_turns.clear();
        for turn_id in turn_ids {
            state
                .outcomes
                .entry(turn_id)
                .or_insert_with(|| TurnOutcome::Failed(message.to_string()));
        }
        self.notify.notify_all();
    }

    #[cfg(test)]
    fn registered_turn_count(&self) -> usize {
        let state = self.state.lock().unwrap();
        state.pending_by_message.len() + state.active_turns.len() + state.outcomes.len()
    }
}

fn remove_turn_state(state: &mut ExecutionTrackerState, turn_id: u64) {
    state
        .pending_by_message
        .retain(|_, pending_turn_id| *pending_turn_id != turn_id);
    state
        .active_turns
        .retain(|active_turn_id| *active_turn_id != turn_id);
    state.outcomes.remove(&turn_id);
}

fn sync_stream_event_to_state(
    state: &SharedState,
    event_bus: &Arc<EventBus>,
    session_id: &str,
    event: &StreamEvent,
) {
    let mut state = state.blocking_lock();
    let completion_event = match event {
        StreamEvent::Done { .. } => Some(true),
        StreamEvent::Error { .. } => Some(false),
        _ => None,
    };

    if let Some(success) = completion_event {
        // Core 只会在最终会话状态落盘后发送终态。此时整会话重载，保留宿主累计的
        // token 指标并覆盖流式 reducer 生成的临时/近似消息。
        if let Err(error) = state.reload_session_from_disk(session_id) {
            tracing::warn!(%error, %session_id, "终态重载 Core 会话失败");
        }
        let _ = state.persist_session(session_id);
        drop(state);
        event_bus.publish(TiangongEvent::TurnCompleted {
            session_id: session_id.to_string(),
            success,
        });
        return;
    }

    let Some(session) = state.sessions_mut().iter_mut().find(|s| s.id == session_id) else {
        return;
    };

    match event {
        StreamEvent::UserMessage {
            message_id,
            content,
            content_blocks,
            media,
            model_excluded,
            pending_agent_deliveries,
        } => {
            apply_user_message_event(
                session,
                message_id,
                content,
                content_blocks,
                media,
                *model_excluded,
            );
            session.pending_agent_deliveries = pending_agent_deliveries.clone();
        }
        StreamEvent::SessionMessageUpsert {
            message,
            pending_agent_deliveries,
            deferred_tool_injections,
        } => {
            if let Some(existing) = session
                .messages
                .iter_mut()
                .find(|existing| existing.id == message.id)
            {
                *existing = message.clone();
            } else {
                session.messages.push(message.clone());
            }
            if let Some(deliveries) = pending_agent_deliveries {
                session.pending_agent_deliveries = deliveries.clone();
            }
            if let Some(injections) = deferred_tool_injections {
                session.deferred_tool_injections = injections.clone();
            }
        }
        StreamEvent::PendingAgentDeliveriesChanged { deliveries } => {
            session.pending_agent_deliveries = deliveries.clone();
        }
        StreamEvent::DeferredToolInjectionsChanged { injections } => {
            session.deferred_tool_injections = injections.clone();
        }
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
        } => append_assistant_delta(session, message_id, content),
        StreamEvent::PhaseChanged { .. } => {}
        StreamEvent::Reasoning {
            message_id,
            content,
        } => append_assistant_reasoning(session, message_id, content),
        StreamEvent::ToolCalls {
            message_id, calls, ..
        } => {
            finalize_assistant_tool_calls(session, message_id, calls);
        }
        StreamEvent::ToolStart { .. } => {
            // 工具开始不再写 System 摘要——避免运行记录污染系统规则
        }
        StreamEvent::ToolResult {
            name,
            tool_call_id,
            ok,
            output,
            full_output,
            duration_ms: _,
        } => {
            let persisted_output = full_output.as_deref().unwrap_or(output);

            append_tool_result_message(
                session,
                tool_call_id.as_deref(),
                name,
                persisted_output.to_string(),
                !*ok,
            );
        }
        StreamEvent::TokenUsage {
            usage,
            current_tokens,
            compression_threshold_tokens,
            context_limit_tokens,
            agent_id,
            ..
        } => {
            if usage.total_tokens > 0 {
                session.token_usage.accumulate(usage);
            }
            if let Some(current_tokens) = current_tokens {
                if let Some(aid) = agent_id {
                    session.active_agent_id = Some(aid.clone());
                    session.active_agent_current_tokens =
                        *current_tokens.max(&session.active_agent_current_tokens);
                } else {
                    session.current_tokens = (*current_tokens).max(session.current_tokens);
                }
            }
            if let Some(compression_threshold_tokens) = compression_threshold_tokens {
                session.compression_threshold_tokens = *compression_threshold_tokens;
            }
            if let Some(context_limit_tokens) = context_limit_tokens {
                session.context_limit_tokens = *context_limit_tokens;
            }
        }
        StreamEvent::ApprovalNeeded { .. } => {}
        StreamEvent::Done { .. } | StreamEvent::Error { .. } => {
            unreachable!("终态已在会话借用前处理")
        }
        StreamEvent::Retry { .. } => {}
        _ => {}
    }
    drop(state);
}

fn apply_user_message_event(
    session: &mut Session,
    message_id: &str,
    content: &str,
    content_blocks: &[tiangong_types::ContentBlock],
    media: &[MediaAsset],
    model_excluded: bool,
) -> bool {
    let existing = session
        .messages
        .iter()
        .position(|message| message.id == message_id && message.role == MessageRole::User)
        .map(|index| session.messages.remove(index));
    let prepared = if !content_blocks.is_empty() {
        PreparedUserMessage::new(content_blocks.to_vec()).stable()
    } else {
        // 兼容旧 Core/历史入口；这里的 media 已由入口归档为稳定本地引用。
        let mut blocks = vec![tiangong_types::ContentBlock::text(content.to_string())];
        blocks.extend(media.iter().map(MediaAsset::to_content_block));
        PreparedUserMessage::new(blocks).stable()
    };
    if let Some(mut message) = existing {
        // 旧 Core 只会重发文本/media 为空的状态事件；此时保留宿主已经同步的
        // 稳定内容块，只更新可见性并把消息移动到当前轮次末尾。
        if !content_blocks.is_empty()
            || !media.is_empty()
            || message.content.is_empty()
            || message.text_content() != content
        {
            message.content = prepared.content;
        }
        message.model_excluded = model_excluded;
        session.messages.push(message);
    } else {
        session.append_prepared_user_message_with_id(message_id.to_string(), prepared);
        session.set_message_model_excluded(message_id, model_excluded);
    }
    true
}

fn append_assistant_delta(session: &mut Session, message_id: &str, content: &str) {
    if content.trim().is_empty()
        && !session
            .messages
            .iter()
            .any(|message| message.id == message_id)
    {
        return;
    }
    ensure_assistant_message(session, message_id);
    if let Some(message) = session
        .messages
        .iter_mut()
        .find(|message| message.id == message_id)
    {
        if message.text_content().trim().is_empty() && content.trim().is_empty() {
            return;
        }
        match message.content.last_mut() {
            Some(tiangong_types::ContentBlock::Text { text }) => text.push_str(content),
            _ => message
                .content
                .push(tiangong_types::ContentBlock::text(content.to_string())),
        }
    }
}

fn append_assistant_reasoning(session: &mut Session, message_id: &str, content: &str) {
    ensure_assistant_message(session, message_id);
    if let Some(message) = session
        .messages
        .iter_mut()
        .find(|message| message.id == message_id)
    {
        message.reasoning_content.push_str(content);
    }
}

fn cleanup_latest_assistant_before_tool_calls(session: &mut Session) {
    let Some(index) = session
        .messages
        .iter()
        .rposition(|message| message.role == MessageRole::Assistant)
    else {
        return;
    };

    let message = &mut session.messages[index];
    if !message.text_content().trim().is_empty() {
        return;
    }
    message.content.clear();
    if message.reasoning_content.trim().is_empty() && !message.has_media() {
        session.messages.remove(index);
    }
}

fn finalize_assistant_tool_calls(
    session: &mut Session,
    message_id: &str,
    calls: &[tiangong_types::StreamToolCall],
) {
    if calls.is_empty() {
        cleanup_latest_assistant_before_tool_calls(session);
        return;
    }
    ensure_assistant_message(session, message_id);
    if let Some(message) = session
        .messages
        .iter_mut()
        .find(|message| message.id == message_id)
    {
        message.tool_calls = calls
            .iter()
            .map(|call| MessageToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect();
    }
}

fn append_tool_result_message(
    session: &mut Session,
    tool_call_id: Option<&str>,
    tool_name: &str,
    content: String,
    is_error: bool,
) {
    let Some(tool_call_id) = tool_call_id else {
        return;
    };
    let current_assistant_index = session.messages.iter().rposition(|message| {
        message.role == MessageRole::Assistant
            && message
                .tool_calls
                .iter()
                .any(|call| call.id == tool_call_id)
    });
    let current_result = current_assistant_index.and_then(|assistant_index| {
        session.messages[assistant_index + 1..]
            .iter()
            .take_while(|message| message.role == MessageRole::Tool)
            .position(|message| message.tool_call_id.as_deref() == Some(tool_call_id))
            .map(|offset| assistant_index + 1 + offset)
    });
    if let Some(message) = current_result.and_then(|index| session.messages.get_mut(index)) {
        message.content = vec![tiangong_types::ContentBlock::text(content)];
        message.tool_name = Some(tool_name.to_string());
        message.tool_result_is_error = is_error;
        session.updated_at = now_text();
        return;
    }
    let message = Message::tool_result(tool_call_id, tool_name, content, is_error);
    session.messages.push(message);
    session.updated_at = now_text();
}

fn ensure_assistant_message(session: &mut Session, message_id: &str) {
    if session
        .messages
        .iter()
        .any(|message| message.id == message_id)
    {
        return;
    }

    session.append_message_with_id(
        message_id.to_string(),
        MessageRole::Assistant,
        String::new(),
        String::new(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use tiangong_core::core::{CoreStorageLocation, Plugin, TiangongCore};
    use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
    use tiangong_core::tool_override::{
        PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
    };
    use tiangong_types::{ContentBlock, StoredAsset};

    static STORAGE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct TestHomeGuard {
        previous_home: Option<OsString>,
    }

    impl TestHomeGuard {
        fn new(home: &Path) -> Self {
            std::fs::create_dir_all(home).unwrap();
            let previous_home = std::env::var_os("HOME");
            // SAFETY: 所有修改 HOME 的 Server 测试均由 STORAGE_TEST_LOCK 串行。
            unsafe { std::env::set_var("HOME", home) };
            Self { previous_home }
        }
    }

    impl Drop for TestHomeGuard {
        fn drop(&mut self) {
            match self.previous_home.take() {
                Some(home) => {
                    // SAFETY: 见 TestHomeGuard::new。
                    unsafe { std::env::set_var("HOME", home) };
                }
                None => {
                    // SAFETY: 见 TestHomeGuard::new。
                    unsafe { std::env::remove_var("HOME") };
                }
            }
        }
    }

    struct BlockingEndPlugin {
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl ToolOverrideHandler for BlockingEndPlugin {}
    impl ToolSpecProvider for BlockingEndPlugin {}
    impl PromptSectionProvider for BlockingEndPlugin {}

    impl Plugin for BlockingEndPlugin {
        fn id(&self) -> &str {
            "blocking-end-test"
        }

        fn on_session_ended(&self, _session: &mut Session) {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            let (released, notify) = &*self.release;
            let mut released = released.lock().unwrap();
            while !*released {
                released = notify.wait(released).unwrap();
            }
        }
    }

    fn isolated_test_manager(
        root: &Path,
    ) -> (Arc<ServerCoreManager>, Session, PathBuf, SharedState) {
        let mut state = tiangong_app_state::app_state::TiangongState::load_or_default();
        let session = state.active_session().cloned().unwrap();
        state.persist_session(&session.id).unwrap();
        let session_path = state
            .services
            .repository
            .paths()
            .sessions_dir_path
            .join(format!("{}.json", session.id));
        let state = Arc::new(AsyncMutex::new(state));
        let manager = Arc::new(ServerCoreManager::new(
            state.clone(),
            CoreConfigProvider::new(CoreConfig::default()),
            Arc::new(EventBus::default()),
            Arc::new(tiangong_plugin_mcp::McpPlugin::with_storage_root(
                root.join("mcp"),
            )),
        ));
        (manager, session, session_path, state)
    }

    fn test_core(
        session: Session,
        storage_root: &Path,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> TiangongCore {
        let (event_tx, _event_rx) = std::sync::mpsc::channel();
        TiangongCore::builder()
            .config(CoreConfigProvider::new(CoreConfig::default()))
            .session(session)
            .event_sender(event_tx)
            .plugins(plugins)
            .storage(CoreStorageLocation::new(storage_root))
            .build()
            .unwrap()
    }

    fn image_asset() -> StoredAsset {
        StoredAsset {
            asset_id: "asset-server".to_string(),
            local_path: "/tmp/server-image.png".to_string(),
            original_name: "server-image.png".to_string(),
            mime_type: "image/png".to_string(),
            size: 4,
            kind: MediaKind::Image,
        }
    }

    #[test]
    fn explicit_missing_session_never_falls_back_to_another_session() {
        let active = Session::new("active");
        let target = Session::new("target");
        let sessions = vec![active.clone(), target.clone()];

        let error = resolve_explicit_session(&sessions, "missing-session").unwrap_err();
        assert!(error.to_string().contains("missing-session"));

        let (resolved_id, resolved) = resolve_explicit_session(&sessions, &target.id).unwrap();
        assert_eq!(resolved_id, target.id);
        assert_eq!(resolved.id, target.id);
        assert_ne!(resolved.id, active.id);
    }

    #[test]
    fn isolated_server_core_config_providers_do_not_share_updates() {
        let config = CoreConfig {
            context_limit: 100,
            ..CoreConfig::default()
        };
        let first = isolated_core_config_provider(&config);
        let second = isolated_core_config_provider(&config);
        let first_snapshot = first.snapshot();
        let second_snapshot = second.snapshot();
        assert!(!Arc::ptr_eq(&first_snapshot, &second_snapshot));

        first.update(|config| config.context_limit = 200);
        assert_eq!(first.snapshot().context_limit, 200);
        assert_eq!(second.snapshot().context_limit, 100);
        assert_eq!(second.generation(), 1);
    }

    #[test]
    fn committed_enqueued_attachments_survive_until_targeted_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let transaction = AttachmentStore::new(root.path().join("archive"))
            .store_batch(vec![RawAttachment {
                kind: MediaKind::File,
                source: "data:text/plain;base64,aGVsbG8=".to_string(),
                mime_type: Some("text/plain".to_string()),
                original_name: Some("hello.txt".to_string()),
            }])
            .unwrap();

        let created_paths = commit_enqueued_attachment_transaction(transaction);
        assert_eq!(created_paths.len(), 1);
        assert!(created_paths[0].is_file());

        let error = attachment_delivery_failure("receipt failed", &created_paths);
        assert!(error.to_string().contains("receipt failed"));
        assert!(!created_paths[0].exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn same_session_install_is_serial_and_never_overwrites_the_winner() {
        let _storage_guard = STORAGE_TEST_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        let _home_guard = TestHomeGuard::new(&root.path().join("home"));
        let (manager, session, _session_path, _state) = isolated_test_manager(root.path());
        let first_core = test_core(session.clone(), root.path(), Vec::new());
        let second_core = test_core(session.clone(), root.path(), Vec::new());
        let first_flag = first_core.cancel_flag();
        let second_flag = second_core.cancel_flag();
        let barrier = Arc::new(tokio::sync::Barrier::new(3));

        let first_task = {
            let manager = manager.clone();
            let session_id = session.id.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                let lock = manager.session_operation_lock(&session_id);
                let _guard = lock.lock().await;
                manager.install_core_if_absent(
                    &session_id,
                    first_core,
                    AttachmentCapabilitySnapshot::default(),
                )
            })
        };
        let second_task = {
            let manager = manager.clone();
            let session_id = session.id.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                let lock = manager.session_operation_lock(&session_id);
                let _guard = lock.lock().await;
                manager.install_core_if_absent(
                    &session_id,
                    second_core,
                    AttachmentCapabilitySnapshot::default(),
                )
            })
        };
        barrier.wait().await;
        let first_installed = first_task.await.unwrap().unwrap();
        let second_installed = second_task.await.unwrap().unwrap();
        assert_ne!(first_installed, second_installed);
        assert_eq!(manager.cores.lock().unwrap().len(), 1);

        let stored_flag = manager
            .cores
            .lock()
            .unwrap()
            .get(&session.id)
            .unwrap()
            .cancel_flag();
        let winner_flag = if first_installed {
            first_flag
        } else {
            second_flag
        };
        assert!(Arc::ptr_eq(&stored_flag, &winner_flag));

        assert!(manager.delete_session(&session.id).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_waits_for_core_join_before_removing_session_state() {
        let _storage_guard = STORAGE_TEST_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        let _home_guard = TestHomeGuard::new(&root.path().join("home"));
        let (manager, session, session_path, state) = isolated_test_manager(root.path());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let plugin = Arc::new(BlockingEndPlugin {
            entered: Mutex::new(Some(entered_tx)),
            release: release.clone(),
        });
        let core = test_core(session.clone(), root.path(), vec![plugin]);
        assert!(
            manager
                .install_core_if_absent(&session.id, core, AttachmentCapabilitySnapshot::default(),)
                .unwrap()
        );
        manager.tracker_for(&session.id);
        manager
            .remote_sessions
            .lock()
            .unwrap()
            .insert("test:channel".to_string(), session.id.clone());

        let delete_task = {
            let manager = manager.clone();
            let session_id = session.id.clone();
            tokio::spawn(async move { manager.delete_session(&session_id).await })
        };
        tokio::time::timeout(Duration::from_secs(5), entered_rx)
            .await
            .expect("Core 应进入结束钩子")
            .expect("结束钩子通知不应丢失");
        assert!(!delete_task.is_finished());
        assert!(manager.cores.lock().unwrap().get(&session.id).is_none());
        assert!(
            state
                .lock()
                .await
                .sessions()
                .iter()
                .any(|candidate| candidate.id == session.id)
        );
        assert!(session_path.is_file());

        {
            let (released, notify) = &*release;
            *released.lock().unwrap() = true;
            notify.notify_all();
        }
        let deleted = tokio::time::timeout(Duration::from_secs(5), delete_task)
            .await
            .expect("删除任务不应超时")
            .expect("删除任务不应 panic")
            .expect("删除会话应成功");
        assert!(deleted);
        assert!(
            !state
                .lock()
                .await
                .sessions()
                .iter()
                .any(|candidate| candidate.id == session.id)
        );
        assert!(!session_path.exists());
        assert!(
            !manager
                .core_attachment_capabilities
                .lock()
                .unwrap()
                .contains_key(&session.id)
        );
        assert!(!manager.trackers.lock().unwrap().contains_key(&session.id));
        assert!(
            !manager
                .remote_sessions
                .lock()
                .unwrap()
                .values()
                .any(|mapped_session_id| mapped_session_id == &session.id)
        );
    }

    #[test]
    fn attachment_preparation_failure_leaves_tracker_empty() {
        let root = tempfile::tempdir().unwrap();
        let tracker = ExecutionTracker::default();
        let raw = vec![RawAttachment {
            kind: MediaKind::Image,
            source: "data:image/png;base64,%%%invalid%%%".to_string(),
            mime_type: Some("image/png".to_string()),
            original_name: Some("broken.png".to_string()),
        }];

        let result = prepare_user_message_blocking(
            root.path().join("media"),
            raw,
            "message-broken".to_string(),
            "broken".to_string(),
            AttachmentCapabilitySnapshot {
                chat_multimodal: true,
                ..AttachmentCapabilitySnapshot::default()
            },
        );

        assert!(result.is_err());
        assert_eq!(tracker.registered_turn_count(), 0);
    }

    #[test]
    fn attachment_preparation_uses_the_server_message_id_in_model_guidance() {
        let root = tempfile::tempdir().unwrap();
        let raw = vec![RawAttachment {
            kind: MediaKind::Image,
            source: "data:image/png;base64,aW1hZ2U=".to_string(),
            mime_type: Some("image/png".to_string()),
            original_name: Some("server.png".to_string()),
        }];

        let (_transaction, prepared) = prepare_user_message_blocking(
            root.path().join("media"),
            raw,
            "message-from-server".to_string(),
            "analyze".to_string(),
            AttachmentCapabilitySnapshot {
                analyze_attachment: true,
                ..AttachmentCapabilitySnapshot::default()
            },
        )
        .unwrap();

        assert!(prepared.content.iter().any(|block| matches!(
            block,
            ContentBlock::ModelInstruction { text }
                if text.contains("message_id=message-from-server")
                    && text.contains("attachment_index=0")
                    && text.contains("analyze_attachment")
        )));
    }

    #[test]
    fn tracker_correlates_events_by_message_and_cleans_cancelled_turn() {
        let tracker = ExecutionTracker::default();
        let first = tracker.start_turn("message-1".to_string());
        let second = tracker.start_turn("message-2".to_string());

        tracker.observe_event(&StreamEvent::UserMessage {
            message_id: "message-2".to_string(),
            content: "second".to_string(),
            content_blocks: Vec::new(),
            media: Vec::new(),
            model_excluded: false,
            pending_agent_deliveries: Vec::new(),
        });
        tracker.observe_event(&StreamEvent::Done { usage: None });
        assert!(matches!(
            tracker.wait_for_turn(second),
            TurnOutcome::Completed
        ));

        tracker.cancel_turn(first);
        assert_eq!(tracker.registered_turn_count(), 0);
    }

    #[test]
    fn tracker_fails_all_waiters_when_core_stream_closes() {
        let tracker = ExecutionTracker::default();
        let pending = tracker.start_turn("message-pending".to_string());
        let active = tracker.start_turn("message-active".to_string());
        tracker.observe_event(&StreamEvent::UserMessage {
            message_id: "message-active".to_string(),
            content: "active".to_string(),
            content_blocks: Vec::new(),
            media: Vec::new(),
            model_excluded: false,
            pending_agent_deliveries: Vec::new(),
        });

        tracker.fail_all("Core 事件流已关闭");

        assert!(matches!(
            tracker.wait_for_turn(pending),
            TurnOutcome::Failed(message) if message.contains("事件流已关闭")
        ));
        assert!(matches!(
            tracker.wait_for_turn(active),
            TurnOutcome::Failed(message) if message.contains("事件流已关闭")
        ));
    }

    #[test]
    fn direct_agent_outgoing_aggregates_all_replies_and_maps_ready_assets() {
        let mut session = Session::new("direct-agent-outgoing");
        session.append_message(MessageRole::User, "@dev @test 请分别处理".to_string());
        session.append_worker_message(MessageRole::Assistant, "开发结果", "agent:dev:agent-dev");
        if let Some(message) = session.messages.last_mut() {
            message.model_excluded = true;
            let mut asset = image_asset();
            asset.local_path = "/tmp/dev-image.png".to_string();
            message.content.push(ContentBlock::AssetReference { asset });
        }
        session.append_worker_message(MessageRole::Assistant, "测试结果", "agent:test:agent-test");
        if let Some(message) = session.messages.last_mut() {
            message.model_excluded = true;
            message.content.push(ContentBlock::AssetReference {
                asset: image_asset(),
            });
        }

        let (outgoing, is_direct) = assistant_outgoing_after_last_user(&session);
        assert!(is_direct);
        match outgoing.content {
            MessageContent::Image { url, caption } => {
                assert_eq!(url, "/tmp/dev-image.png");
                let caption = caption.unwrap();
                assert!(caption.contains("开发结果"));
                assert!(caption.contains("测试结果"));
            }
            other => panic!("expected image outgoing, got {other:?}"),
        }
        assert_eq!(outgoing.attachments.len(), 1);
        assert!(matches!(
            &outgoing.attachments[0],
            MessageContent::Image { url, .. } if url == "/tmp/server-image.png"
        ));
    }

    #[test]
    fn user_message_event_preserves_exact_stable_content_blocks() {
        let mut session = Session::new("server-state");
        let content_blocks = vec![
            ContentBlock::Text {
                text: "event text".to_string(),
            },
            ContentBlock::AssetReference {
                asset: image_asset(),
            },
            ContentBlock::ModelInstruction {
                text: "model-only attachment guidance".to_string(),
            },
        ];

        assert!(apply_user_message_event(
            &mut session,
            "message-server",
            "event text",
            &content_blocks,
            &[],
            true,
        ));
        let message = session.messages.last().unwrap();
        assert_eq!(message.text_content(), "event text");
        assert_eq!(message.content, content_blocks);
        assert!(message.model_excluded);

        assert!(apply_user_message_event(
            &mut session,
            "message-server",
            "event text",
            &[],
            &[],
            false,
        ));
        assert!(!session.messages.last().unwrap().model_excluded);
        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains("\"type\":\"asset_reference\""));
        assert!(json.contains("model-only attachment guidance"));
    }

    #[test]
    fn tool_result_updates_only_the_latest_matching_call_batch() {
        let mut session = Session::new("reused-tool-id");
        let mut first = Message::new(MessageRole::Assistant, String::new());
        first.tool_calls = vec![MessageToolCall {
            id: "reused".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({}),
        }];
        session.messages.push(first);
        append_tool_result_message(
            &mut session,
            Some("reused"),
            "read_file",
            "old-result".to_string(),
            false,
        );
        session.append_message(MessageRole::User, "next");
        let mut second = Message::new(MessageRole::Assistant, String::new());
        second.tool_calls = vec![MessageToolCall {
            id: "reused".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({}),
        }];
        session.messages.push(second);

        append_tool_result_message(
            &mut session,
            Some("reused"),
            "read_file",
            "new-result".to_string(),
            false,
        );
        let result_indices = session
            .messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.tool_call_id.as_deref() == Some("reused"))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(result_indices.len(), 2);
        assert_eq!(
            session.messages[result_indices[0]].text_content(),
            "old-result"
        );
        assert_eq!(
            session.messages[result_indices[1]].text_content(),
            "new-result"
        );

        let len = session.messages.len();
        append_tool_result_message(
            &mut session,
            Some("reused"),
            "read_file",
            "new-result-updated".to_string(),
            false,
        );
        assert_eq!(session.messages.len(), len);
        assert_eq!(
            session.messages[result_indices[1]].text_content(),
            "new-result-updated"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prepared_receipt_confirms_stable_message_persistence() {
        let _guard = STORAGE_TEST_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        tiangong_core::storage::set_storage_root(root.path().to_path_buf());
        let session = Session::new("receipt");
        let session_id = session.id.clone();
        let (event_tx, _event_rx) = std::sync::mpsc::channel();
        let core = TiangongCore::builder()
            .config(CoreConfigProvider::new(CoreConfig::default()))
            .session(session)
            .event_sender(event_tx)
            .storage(CoreStorageLocation::new(root.path()))
            .build()
            .unwrap();

        let receipt = core
            .enqueue_prepared_with_receipt(
                "receipt-message",
                PreparedUserMessage::new(vec![
                    ContentBlock::Text {
                        text: "hello".to_string(),
                    },
                    ContentBlock::Image {
                        asset: image_asset(),
                        data: Some("RECEIPT_RUNTIME_SECRET".to_string()),
                    },
                ]),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), receipt.await_persisted())
            .await
            .expect("持久化确认不应超时")
            .expect("Prepared 消息应持久化成功");

        let json = std::fs::read_to_string(
            root.path()
                .join("sessions")
                .join(format!("{session_id}.json")),
        )
        .unwrap();
        assert!(json.contains("receipt-message"));
        assert!(json.contains("\"type\": \"image\""));
        assert!(json.contains("asset-server"));
        assert!(!json.contains("RECEIPT_RUNTIME_SECRET"));

        drop(core);
    }
}
