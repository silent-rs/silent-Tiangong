use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::core::model::{ModelProviderConfig, SingleProviderClient};
use crate::core::planner::TaskPlan;
use crate::core::runtime::{RunSnapshot, RunStatus, RuntimeEngine, TurnExecution};
use crate::core::session::{Message, MessageRole, Session, now_text};
use crate::core::tool::ToolExecutionRecord;

const DEFAULT_SESSION_TITLE: &str = "默认会话";
const DEFAULT_CONTEXT_LIMIT: usize = 16;

#[derive(Debug, Serialize, Deserialize, Default)]
struct PersistedAppState {
    #[serde(default)]
    active_session_id: String,
    #[serde(default)]
    session_ids: Vec<String>,
    #[serde(default)]
    model_config: Option<ModelProviderConfig>,
    #[serde(default)]
    model_list: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LegacyPersistedState {
    sessions: Vec<Session>,
    active_session_id: String,
    #[serde(default)]
    model_config: Option<ModelProviderConfig>,
    #[serde(default)]
    model_list: Vec<String>,
}

#[derive(Debug)]
struct LoadedState {
    sessions: Vec<Session>,
    active_session_id: String,
    model_config: Option<ModelProviderConfig>,
    model_list: Vec<String>,
}

#[derive(Debug)]
enum TurnEvent {
    Chunk(String),
    Completed(Box<TurnExecution>),
    Failed(String),
}

#[derive(Debug)]
struct PendingTurn {
    session_id: String,
    task_id: String,
    assistant_message_id: String,
    started_at: Instant,
    rx: Receiver<TurnEvent>,
}

#[derive(Debug)]
pub struct TiangongState {
    sessions: Vec<Session>,
    active_session_id: String,
    model_config: ModelProviderConfig,
    session_title_draft: String,
    settings_api_auth_token_draft: String,
    settings_api_base_url_draft: String,
    settings_api_timeout_ms_draft: String,
    settings_api_model_draft: String,
    settings_model_list: Vec<String>,
    pub input_draft: String,
    pub run: RunSnapshot,
    pending_turn: Option<PendingTurn>,
    app_storage_path: PathBuf,
    sessions_dir_path: PathBuf,
    runtime: RuntimeEngine,
}

impl Default for TiangongState {
    fn default() -> Self {
        Self::load_or_default()
    }
}

impl TiangongState {
    pub fn load_or_default() -> Self {
        let app_storage_path = default_app_storage_path();
        let sessions_dir_path = default_sessions_dir_path();
        let default_model_config = ModelProviderConfig::from_env();
        let runtime = RuntimeEngine::new(
            SingleProviderClient::new(default_model_config.clone()),
            DEFAULT_CONTEXT_LIMIT,
        );

        let mut state = Self {
            sessions: Vec::new(),
            active_session_id: String::new(),
            model_config: default_model_config.clone(),
            session_title_draft: DEFAULT_SESSION_TITLE.to_string(),
            settings_api_auth_token_draft: default_model_config.api_auth_token.clone(),
            settings_api_base_url_draft: default_model_config.api_base_url.clone(),
            settings_api_timeout_ms_draft: default_model_config.api_timeout_ms.clone(),
            settings_api_model_draft: default_model_config.api_model.clone(),
            settings_model_list: Vec::new(),
            input_draft: String::new(),
            run: RunSnapshot::default(),
            pending_turn: None,
            app_storage_path,
            sessions_dir_path,
            runtime,
        };

        if let Ok(Some(loaded)) = state.load_from_disk() {
            state.apply_loaded_state(loaded);
        } else if let Ok(Some(legacy_loaded)) = state.load_from_legacy_disk() {
            state.apply_loaded_state(legacy_loaded);
            let _ = state.persist_to_disk();
        }

        if state.sessions.is_empty() {
            let session = Session::new(DEFAULT_SESSION_TITLE);
            state.active_session_id = session.id.clone();
            state.sessions.push(session);
            let _ = state.persist_to_disk();
        }

        if !state
            .sessions
            .iter()
            .any(|session| session.id == state.active_session_id)
        {
            state.active_session_id = state
                .sessions
                .first()
                .map(|session| session.id.clone())
                .unwrap_or_default();
        }

        state.settings_api_auth_token_draft = state.model_config.api_auth_token.clone();
        state.settings_api_base_url_draft = state.model_config.api_base_url.clone();
        state.settings_api_timeout_ms_draft = state.model_config.api_timeout_ms.clone();
        state.settings_api_model_draft = state.model_config.api_model.clone();
        state.session_title_draft = state
            .active_session()
            .map(|session| session.title.clone())
            .unwrap_or_else(|| DEFAULT_SESSION_TITLE.to_string());
        state.settings_model_list = normalize_model_list(
            state.settings_model_list.clone(),
            &state.model_config.api_model,
        );

        state.run.summary = format!("模型供应商：{}", state.runtime.provider_label());
        state.run.updated_at = now_text();

        state
    }

    fn apply_loaded_state(&mut self, loaded: LoadedState) {
        self.sessions = loaded.sessions;
        self.active_session_id = loaded.active_session_id;
        self.settings_model_list = loaded.model_list;
        if let Some(model_config) = loaded.model_config {
            self.model_config = model_config;
            self.runtime = RuntimeEngine::new(
                SingleProviderClient::new(self.model_config.clone()),
                DEFAULT_CONTEXT_LIMIT,
            );
        }
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    pub fn active_session_id(&self) -> &str {
        &self.active_session_id
    }

    pub fn active_session(&self) -> Option<&Session> {
        self.sessions
            .iter()
            .find(|session| session.id == self.active_session_id)
    }

    pub fn has_pending_turn(&self) -> bool {
        self.pending_turn.is_some()
    }

    pub fn poll_pending_turn(&mut self) {
        let mut should_clear = false;
        let mut disconnected = false;

        while let Some(event) = self.try_recv_turn_event(&mut disconnected) {
            match event {
                TurnEvent::Chunk(delta) => {
                    self.apply_assistant_delta(&delta);
                }
                TurnEvent::Completed(exec) => {
                    self.finish_pending_turn_success(*exec);
                    should_clear = true;
                }
                TurnEvent::Failed(err_msg) => {
                    self.finish_pending_turn_error(&err_msg);
                    should_clear = true;
                }
            }
        }

        if disconnected && !should_clear {
            self.finish_pending_turn_error("执行中断：后台任务通道已关闭");
            should_clear = true;
        }

        if should_clear {
            self.pending_turn = None;
        }
    }

    pub fn create_session(&mut self) {
        let title = format!("会话 {}", self.sessions.len() + 1);
        let session = Session::new(title);
        self.active_session_id = session.id.clone();
        self.session_title_draft = session.title.clone();
        let session_id = session.id.clone();
        self.sessions.push(session);
        let _ = self.persist_session_and_app(&session_id);
    }

    pub fn switch_session(&mut self, session_id: &str) {
        if let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        {
            self.active_session_id = session_id.to_string();
            self.session_title_draft = session.title.clone();
            let _ = self.persist_app_only();
        }
    }

    pub fn update_draft(&mut self, value: String) {
        self.input_draft = value;
    }

    pub fn provider_label(&self) -> String {
        self.runtime.provider_label()
    }

    pub fn model_config(&self) -> &ModelProviderConfig {
        &self.model_config
    }

    pub fn settings_api_auth_token_draft(&self) -> &str {
        &self.settings_api_auth_token_draft
    }

    pub fn session_title_draft(&self) -> &str {
        &self.session_title_draft
    }

    pub fn update_session_title_draft(&mut self, value: String) {
        self.session_title_draft = value;
    }

    pub fn save_active_session_title(&mut self) -> Result<()> {
        let new_title = self.session_title_draft.trim();
        if new_title.is_empty() {
            return Err(anyhow!("会话标题不能为空"));
        }

        let active_id = self.active_session_id.clone();
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
        else {
            return Err(anyhow!("当前会话不存在，无法重命名"));
        };

        session.title = new_title.to_string();
        session.updated_at = now_text();
        self.session_title_draft = session.title.clone();
        self.persist_session_and_app(&active_id)
    }

    pub fn delete_active_session(&mut self) -> Result<()> {
        let active_id = self.active_session_id.clone();
        let Some(remove_idx) = self
            .sessions
            .iter()
            .position(|session| session.id == active_id)
        else {
            return Err(anyhow!("当前会话不存在，无法删除"));
        };

        self.sessions.remove(remove_idx);

        if self.sessions.is_empty() {
            let session = Session::new(DEFAULT_SESSION_TITLE);
            self.active_session_id = session.id.clone();
            self.session_title_draft = session.title.clone();
            self.sessions.push(session);
        } else {
            let next_idx = if remove_idx >= self.sessions.len() {
                self.sessions.len() - 1
            } else {
                remove_idx
            };
            self.active_session_id = self.sessions[next_idx].id.clone();
            self.session_title_draft = self.sessions[next_idx].title.clone();
        }

        self.remove_session_file(&active_id)?;
        if self.sessions.len() == 1 && self.sessions[0].messages.is_empty() {
            let current_id = self.sessions[0].id.clone();
            self.persist_session(&current_id)?;
        }
        self.persist_app_only()
    }

    pub fn settings_api_base_url_draft(&self) -> &str {
        &self.settings_api_base_url_draft
    }

    pub fn settings_api_timeout_ms_draft(&self) -> &str {
        &self.settings_api_timeout_ms_draft
    }

    pub fn settings_api_model_draft(&self) -> &str {
        &self.settings_api_model_draft
    }

    pub fn settings_model_list(&self) -> &[String] {
        &self.settings_model_list
    }

    pub fn model_list(&self) -> &[String] {
        &self.settings_model_list
    }

    pub fn current_model(&self) -> &str {
        &self.model_config.api_model
    }

    pub fn select_model(&mut self, model: &str) -> Result<()> {
        let api_model = model.trim();
        if api_model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空"));
        }

        self.model_config.api_model = api_model.to_string();
        self.settings_api_model_draft = api_model.to_string();
        self.settings_model_list = normalize_model_list(
            self.settings_model_list.clone(),
            &self.model_config.api_model,
        );
        self.runtime = RuntimeEngine::new(
            SingleProviderClient::new(self.model_config.clone()),
            DEFAULT_CONTEXT_LIMIT,
        );
        self.run = RunSnapshot {
            status: RunStatus::Idle,
            summary: format!("模型已切换：{}", self.model_config.api_model),
            last_session_id: self.run.last_session_id.clone(),
            last_task_id: self.run.last_task_id.clone(),
            last_duration_ms: self.run.last_duration_ms,
            last_result: self.run.last_result.clone(),
            last_plan: self.run.last_plan.clone(),
            last_tool_result: self.run.last_tool_result.clone(),
            last_error: None,
            last_usage: self.run.last_usage.clone(),
            updated_at: now_text(),
        };
        self.persist_app_only()
    }

    pub fn open_provider_settings(&mut self) {
        self.settings_api_auth_token_draft = self.model_config.api_auth_token.clone();
        self.settings_api_base_url_draft = self.model_config.api_base_url.clone();
        self.settings_api_timeout_ms_draft = self.model_config.api_timeout_ms.clone();
        self.settings_api_model_draft = self.model_config.api_model.clone();
    }

    pub fn update_settings_api_auth_token_draft(&mut self, value: String) {
        self.settings_api_auth_token_draft = value;
    }

    pub fn update_settings_api_base_url_draft(&mut self, value: String) {
        self.settings_api_base_url_draft = value;
    }

    pub fn update_settings_api_timeout_ms_draft(&mut self, value: String) {
        self.settings_api_timeout_ms_draft = value;
    }

    pub fn update_settings_api_model_draft(&mut self, value: String) {
        self.settings_api_model_draft = value;
    }

    pub fn refresh_model_list(&mut self) -> Result<usize> {
        let draft_config = ModelProviderConfig {
            api_auth_token: self.settings_api_auth_token_draft.trim().to_string(),
            api_base_url: self.settings_api_base_url_draft.trim().to_string(),
            api_timeout_ms: self.settings_api_timeout_ms_draft.trim().to_string(),
            api_model: self.settings_api_model_draft.trim().to_string(),
        };

        let models = SingleProviderClient::list_models(&draft_config)?;
        self.settings_model_list = models;
        let draft_model = self.settings_api_model_draft.trim();
        let need_fill_default =
            draft_model.is_empty() || !self.settings_model_list.iter().any(|m| m == draft_model);
        if need_fill_default && let Some(first) = self.settings_model_list.first() {
            self.settings_api_model_draft = first.clone();
        }
        self.settings_model_list = normalize_model_list(
            self.settings_model_list.clone(),
            self.settings_api_model_draft.trim(),
        );
        self.persist_app_only()?;

        Ok(self.settings_model_list.len())
    }

    pub fn discard_provider_settings(&mut self) {
        self.settings_api_auth_token_draft = self.model_config.api_auth_token.clone();
        self.settings_api_base_url_draft = self.model_config.api_base_url.clone();
        self.settings_api_timeout_ms_draft = self.model_config.api_timeout_ms.clone();
        self.settings_api_model_draft = self.model_config.api_model.clone();
    }

    pub fn save_provider_settings(&mut self) -> Result<()> {
        let api_auth_token = self.settings_api_auth_token_draft.trim();
        let api_base_url = self.settings_api_base_url_draft.trim();
        let api_timeout_ms = self.settings_api_timeout_ms_draft.trim();
        let api_model = self.settings_api_model_draft.trim();

        if api_auth_token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空"));
        }
        if api_base_url.is_empty() {
            return Err(anyhow!("API_BASE_URL 不能为空"));
        }
        if api_timeout_ms.is_empty() {
            return Err(anyhow!("API_TIMEOUT_MS 不能为空"));
        }
        if api_timeout_ms.parse::<u64>().is_err() {
            return Err(anyhow!("API_TIMEOUT_MS 必须是毫秒数值"));
        }
        if api_model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空"));
        }

        self.model_config = ModelProviderConfig {
            api_auth_token: api_auth_token.to_string(),
            api_base_url: api_base_url.to_string(),
            api_timeout_ms: api_timeout_ms.to_string(),
            api_model: api_model.to_string(),
        };
        self.settings_model_list = normalize_model_list(
            self.settings_model_list.clone(),
            &self.model_config.api_model,
        );
        self.runtime = RuntimeEngine::new(
            SingleProviderClient::new(self.model_config.clone()),
            DEFAULT_CONTEXT_LIMIT,
        );
        self.run = RunSnapshot {
            status: RunStatus::Idle,
            summary: format!("模型供应商已更新：{}", self.runtime.provider_label()),
            last_session_id: self.run.last_session_id.clone(),
            last_task_id: self.run.last_task_id.clone(),
            last_duration_ms: self.run.last_duration_ms,
            last_result: self.run.last_result.clone(),
            last_plan: self.run.last_plan.clone(),
            last_tool_result: self.run.last_tool_result.clone(),
            last_error: None,
            last_usage: self.run.last_usage.clone(),
            updated_at: now_text(),
        };
        self.persist_app_only()
    }

    pub fn send_current_input(&mut self) -> Result<()> {
        if self.pending_turn.is_some() {
            return Ok(());
        }

        let input = self.input_draft.trim().to_string();
        if input.is_empty() {
            return Ok(());
        }

        let active_idx = self.ensure_active_session_index();
        let session_id = self.sessions[active_idx].id.clone();
        let task_id = new_scru128_string();
        self.sessions[active_idx].append_message(MessageRole::User, input.clone());
        self.sessions[active_idx].append_message(MessageRole::Assistant, String::new());
        let assistant_message_id = self.sessions[active_idx]
            .messages
            .last()
            .map(|msg| msg.id.clone())
            .ok_or_else(|| anyhow!("创建助手消息占位失败"))?;

        self.run = RunSnapshot {
            status: RunStatus::Planning,
            summary: "正在生成执行计划".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id.clone()),
            last_duration_ms: None,
            last_result: None,
            last_plan: None,
            last_tool_result: None,
            last_error: None,
            last_usage: None,
            updated_at: now_text(),
        };

        self.run = RunSnapshot {
            status: RunStatus::Executing,
            summary: "正在流式调用模型".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id.clone()),
            last_duration_ms: None,
            last_result: None,
            last_plan: None,
            last_tool_result: None,
            last_error: None,
            last_usage: None,
            updated_at: now_text(),
        };

        self.input_draft.clear();

        let runtime = self.runtime.clone();
        let session_snapshot = self.sessions[active_idx].clone();
        let worker_input = input.clone();
        let (tx, rx) = mpsc::channel::<TurnEvent>();

        thread::spawn(move || {
            let chunk_tx = tx.clone();
            let result =
                runtime.execute_turn_with_streaming(&session_snapshot, &worker_input, |delta| {
                    let _ = chunk_tx.send(TurnEvent::Chunk(delta.to_string()));
                });

            match result {
                Ok(exec) => {
                    let _ = tx.send(TurnEvent::Completed(Box::new(exec)));
                }
                Err(err) => {
                    let _ = tx.send(TurnEvent::Failed(RuntimeEngine::fallback_error_message(
                        &err,
                    )));
                }
            }
        });

        self.pending_turn = Some(PendingTurn {
            session_id,
            task_id,
            assistant_message_id,
            started_at: Instant::now(),
            rx,
        });

        Ok(())
    }

    fn try_recv_turn_event(&mut self, disconnected: &mut bool) -> Option<TurnEvent> {
        let pending = self.pending_turn.as_ref()?;

        match pending.rx.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                *disconnected = true;
                None
            }
        }
    }

    fn apply_assistant_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }

        let Some((session_id, assistant_message_id)) = self.pending_turn.as_ref().map(|pending| {
            (
                pending.session_id.clone(),
                pending.assistant_message_id.clone(),
            )
        }) else {
            return;
        };

        if let Some(message) = self.find_message_mut(&session_id, &assistant_message_id) {
            message.content.push_str(delta);
        }
    }

    fn finish_pending_turn_success(&mut self, exec: TurnExecution) {
        let Some((session_id, task_id, assistant_message_id, started_at)) =
            self.pending_turn.as_ref().map(|pending| {
                (
                    pending.session_id.clone(),
                    pending.task_id.clone(),
                    pending.assistant_message_id.clone(),
                    pending.started_at,
                )
            })
        else {
            return;
        };

        if let Some(message) = self.find_message_mut(&session_id, &assistant_message_id) {
            if message.content.trim().is_empty() {
                message.content = exec.assistant_message.clone();
            }
        } else if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.append_message(MessageRole::Assistant, exec.assistant_message.clone());
        }

        let tool_result_text =
            merge_tool_result_text(exec.tool_result_summary, exec.tool_execution.as_ref());

        self.run = RunSnapshot {
            status: RunStatus::Completed,
            summary: "执行完成".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id),
            last_duration_ms: Some(elapsed_ms_u64(started_at.elapsed().as_millis())),
            last_result: Some(format!(
                "success; output_mode={}; chunks={}",
                exec.output_mode, exec.output_chunk_count
            )),
            last_plan: Some(format_plan_snapshot(&exec.plan)),
            last_tool_result: tool_result_text,
            last_error: None,
            last_usage: Some(exec.usage),
            updated_at: now_text(),
        };

        if let Err(err) = self.persist_session_and_app(&session_id) {
            self.run = RunSnapshot {
                status: RunStatus::Failed,
                summary: "会话持久化失败".to_string(),
                last_session_id: self.run.last_session_id.clone(),
                last_task_id: self.run.last_task_id.clone(),
                last_duration_ms: self.run.last_duration_ms,
                last_result: Some("failed".to_string()),
                last_plan: self.run.last_plan.clone(),
                last_tool_result: self.run.last_tool_result.clone(),
                last_error: Some(err.to_string()),
                last_usage: self.run.last_usage.clone(),
                updated_at: now_text(),
            };
        }
    }

    fn finish_pending_turn_error(&mut self, err_msg: &str) {
        let Some((session_id, task_id, assistant_message_id, started_at)) =
            self.pending_turn.as_ref().map(|pending| {
                (
                    pending.session_id.clone(),
                    pending.task_id.clone(),
                    pending.assistant_message_id.clone(),
                    pending.started_at,
                )
            })
        else {
            return;
        };

        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if let Some(position) = session
                .messages
                .iter()
                .position(|msg| msg.id == assistant_message_id && msg.content.trim().is_empty())
            {
                session.messages.remove(position);
                session.updated_at = now_text();
            }
            session.append_message(MessageRole::System, err_msg);
        }

        self.run = RunSnapshot {
            status: RunStatus::Failed,
            summary: "执行失败".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id),
            last_duration_ms: Some(elapsed_ms_u64(started_at.elapsed().as_millis())),
            last_result: Some("failed".to_string()),
            last_plan: None,
            last_tool_result: None,
            last_error: Some(err_msg.to_string()),
            last_usage: None,
            updated_at: now_text(),
        };

        if let Err(err) = self.persist_session_and_app(&session_id) {
            self.run = RunSnapshot {
                status: RunStatus::Failed,
                summary: "会话持久化失败".to_string(),
                last_session_id: self.run.last_session_id.clone(),
                last_task_id: self.run.last_task_id.clone(),
                last_duration_ms: self.run.last_duration_ms,
                last_result: Some("failed".to_string()),
                last_plan: self.run.last_plan.clone(),
                last_tool_result: self.run.last_tool_result.clone(),
                last_error: Some(err.to_string()),
                last_usage: self.run.last_usage.clone(),
                updated_at: now_text(),
            };
        }
    }

    fn find_message_mut(&mut self, session_id: &str, message_id: &str) -> Option<&mut Message> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == session_id)?
            .messages
            .iter_mut()
            .find(|msg| msg.id == message_id)
    }

    fn persist_session_and_app(&mut self, session_id: &str) -> Result<()> {
        self.persist_session(session_id)?;
        self.persist_app_only()
    }

    fn persist_session(&mut self, session_id: &str) -> Result<()> {
        self.normalize_sessions_for_storage();
        ensure_dir(&self.sessions_dir_path)?;

        let session = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("会话不存在，无法持久化：{session_id}"))?;

        let session_path = session_storage_path(&self.sessions_dir_path, session_id);
        let content = serde_json::to_string_pretty(session)
            .with_context(|| format!("序列化会话失败：{session_id}"))?;
        fs::write(&session_path, content)
            .with_context(|| format!("写入会话文件失败：{}", session_path.display()))
    }

    fn persist_app_only(&mut self) -> Result<()> {
        self.normalize_sessions_for_storage();
        ensure_parent_dir(&self.app_storage_path)?;

        let payload = PersistedAppState {
            active_session_id: self.active_session_id.clone(),
            session_ids: self
                .sessions
                .iter()
                .map(|session| session.id.clone())
                .collect(),
            model_config: Some(self.model_config.clone()),
            model_list: self.settings_model_list.clone(),
        };
        let content = serde_json::to_string_pretty(&payload).context("序列化应用存储失败")?;
        fs::write(&self.app_storage_path, content)
            .with_context(|| format!("写入应用存储失败：{}", self.app_storage_path.display()))
    }

    fn remove_session_file(&self, session_id: &str) -> Result<()> {
        let session_path = session_storage_path(&self.sessions_dir_path, session_id);
        if session_path.exists() {
            fs::remove_file(&session_path)
                .with_context(|| format!("删除会话文件失败：{}", session_path.display()))?;
        }
        Ok(())
    }

    fn ensure_active_session_index(&mut self) -> usize {
        if let Some(idx) = self
            .sessions
            .iter()
            .position(|session| session.id == self.active_session_id)
        {
            return idx;
        }

        let session = Session::new(DEFAULT_SESSION_TITLE);
        self.active_session_id = session.id.clone();
        self.sessions.push(session);
        self.sessions.len() - 1
    }

    fn load_from_disk(&self) -> Result<Option<LoadedState>> {
        if !self.app_storage_path.exists() {
            let session_ids = self.list_session_ids_from_dir()?;
            if session_ids.is_empty() {
                return Ok(None);
            }

            let mut sessions = Vec::new();
            for session_id in &session_ids {
                if let Some(session) = self.load_session_from_disk(session_id)? {
                    sessions.push(session);
                }
            }

            return Ok(Some(LoadedState {
                sessions,
                active_session_id: session_ids.first().cloned().unwrap_or_default(),
                model_config: None,
                model_list: Vec::new(),
            }));
        }

        let content = fs::read_to_string(&self.app_storage_path)
            .with_context(|| format!("读取应用存储失败：{}", self.app_storage_path.display()))?;
        let persisted: PersistedAppState =
            serde_json::from_str(&content).context("解析应用存储失败")?;

        let mut session_ids = dedup_session_ids(persisted.session_ids);
        if session_ids.is_empty() {
            session_ids = self.list_session_ids_from_dir()?;
        }

        let mut sessions = Vec::new();
        for session_id in &session_ids {
            if let Some(session) = self.load_session_from_disk(session_id)? {
                sessions.push(session);
            }
        }

        Ok(Some(LoadedState {
            sessions,
            active_session_id: persisted.active_session_id,
            model_config: persisted.model_config,
            model_list: persisted.model_list,
        }))
    }

    fn load_from_legacy_disk(&self) -> Result<Option<LoadedState>> {
        let legacy_storage_path = default_legacy_storage_path();
        if !legacy_storage_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&legacy_storage_path)
            .with_context(|| format!("读取旧会话存储失败：{}", legacy_storage_path.display()))?;
        let persisted: LegacyPersistedState =
            serde_json::from_str(&content).context("解析旧会话存储失败")?;

        Ok(Some(LoadedState {
            sessions: persisted.sessions,
            active_session_id: persisted.active_session_id,
            model_config: persisted.model_config,
            model_list: persisted.model_list,
        }))
    }

    fn load_session_from_disk(&self, session_id: &str) -> Result<Option<Session>> {
        let session_path = session_storage_path(&self.sessions_dir_path, session_id);
        if !session_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&session_path)
            .with_context(|| format!("读取会话文件失败：{}", session_path.display()))?;
        let mut session: Session = serde_json::from_str(&content)
            .with_context(|| format!("解析会话文件失败：{}", session_path.display()))?;
        session.id = session_id.to_string();
        Ok(Some(session))
    }

    fn list_session_ids_from_dir(&self) -> Result<Vec<String>> {
        if !self.sessions_dir_path.exists() {
            return Ok(Vec::new());
        }

        let mut session_ids = Vec::new();
        for entry in fs::read_dir(&self.sessions_dir_path)
            .with_context(|| format!("读取会话目录失败：{}", self.sessions_dir_path.display()))?
        {
            let entry = entry.with_context(|| {
                format!("读取会话目录项失败：{}", self.sessions_dir_path.display())
            })?;

            let file_type = entry.file_type().with_context(|| {
                format!(
                    "读取会话目录项类型失败：{}",
                    self.sessions_dir_path.display()
                )
            })?;
            if !file_type.is_file() {
                continue;
            }

            let path = entry.path();
            if path.extension() != Some(OsStr::new("json")) {
                continue;
            }

            let Some(file_stem) = path.file_stem().and_then(|v| v.to_str()) else {
                continue;
            };

            if let Some(session_id) = canonical_scru128_id(file_stem) {
                session_ids.push(session_id);
            }
        }

        session_ids.sort();
        session_ids.dedup();
        Ok(session_ids)
    }

    fn persist_to_disk(&mut self) -> Result<()> {
        self.normalize_sessions_for_storage();

        ensure_dir(&self.sessions_dir_path)?;
        ensure_parent_dir(&self.app_storage_path)?;

        let mut session_ids_set = HashSet::new();

        for session in &self.sessions {
            let session_path = session_storage_path(&self.sessions_dir_path, &session.id);
            let content = serde_json::to_string_pretty(session)
                .with_context(|| format!("序列化会话失败：{}", session.id))?;
            fs::write(&session_path, content)
                .with_context(|| format!("写入会话文件失败：{}", session_path.display()))?;
            session_ids_set.insert(session.id.clone());
        }

        for session_id in self.list_session_ids_from_dir()? {
            if session_ids_set.contains(&session_id) {
                continue;
            }

            let stale_path = session_storage_path(&self.sessions_dir_path, &session_id);
            if stale_path.exists() {
                fs::remove_file(&stale_path)
                    .with_context(|| format!("删除废弃会话文件失败：{}", stale_path.display()))?;
            }
        }

        let payload = PersistedAppState {
            active_session_id: self.active_session_id.clone(),
            session_ids: self
                .sessions
                .iter()
                .map(|session| session.id.clone())
                .collect(),
            model_config: Some(self.model_config.clone()),
            model_list: self.settings_model_list.clone(),
        };
        let content = serde_json::to_string_pretty(&payload).context("序列化应用存储失败")?;
        fs::write(&self.app_storage_path, content)
            .with_context(|| format!("写入应用存储失败：{}", self.app_storage_path.display()))
    }

    fn normalize_sessions_for_storage(&mut self) {
        let mut seen = HashSet::new();

        for session in &mut self.sessions {
            let mut session_id =
                canonical_scru128_id(&session.id).unwrap_or_else(new_scru128_string);
            while seen.contains(&session_id) {
                session_id = new_scru128_string();
            }
            session.id = session_id.clone();
            seen.insert(session_id);
        }

        if self.sessions.is_empty() {
            self.active_session_id.clear();
            return;
        }

        if let Some(active_id) = canonical_scru128_id(&self.active_session_id)
            && seen.contains(&active_id)
        {
            self.active_session_id = active_id;
            return;
        }

        self.active_session_id = self
            .sessions
            .first()
            .map(|session| session.id.clone())
            .unwrap_or_default();
    }
}

fn default_storage_root() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".tiangong")
}

fn default_app_storage_path() -> PathBuf {
    default_storage_root().join("app.json")
}

fn default_sessions_dir_path() -> PathBuf {
    default_storage_root().join("sessions")
}

fn default_legacy_storage_path() -> PathBuf {
    default_storage_root().join("sessions.json")
}

fn session_storage_path(sessions_dir: &Path, session_id: &str) -> PathBuf {
    sessions_dir.join(format!("{session_id}.json"))
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败：{}", parent.display()))?;
    }
    Ok(())
}

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("创建目录失败：{}", path.display()))
}

fn dedup_session_ids(raw_ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();

    for raw_id in raw_ids {
        let Some(session_id) = canonical_scru128_id(&raw_id) else {
            continue;
        };
        if seen.insert(session_id.clone()) {
            ids.push(session_id);
        }
    }

    ids
}

fn canonical_scru128_id(raw: &str) -> Option<String> {
    raw.trim()
        .parse::<scru128::Scru128Id>()
        .ok()
        .map(|id| id.to_string())
}

fn new_scru128_string() -> String {
    scru128::new().to_string()
}

fn elapsed_ms_u64(value: u128) -> u64 {
    value.min(u128::from(u64::MAX)) as u64
}

fn merge_tool_result_text(
    base: Option<String>,
    record: Option<&ToolExecutionRecord>,
) -> Option<String> {
    match (base, record) {
        (Some(base), Some(record)) => Some(format!(
            "{base} | args={} | ok={}",
            record.args.join(" "),
            record.ok
        )),
        (Some(base), None) => Some(base),
        (None, Some(record)) => Some(format!(
            "{} | args={} | ok={}",
            record.summary,
            record.args.join(" "),
            record.ok
        )),
        (None, None) => None,
    }
}

fn format_plan_snapshot(plan: &TaskPlan) -> String {
    let risks = if plan.risks.is_empty() {
        "无".to_string()
    } else {
        plan.risks.join("；")
    };
    let verify_commands = if plan.verify_commands.is_empty() {
        "无".to_string()
    } else {
        plan.verify_commands.join("；")
    };

    format!(
        "{}\n目标：{}\n步骤数：{}\n风险：{}\n验证命令：{}",
        plan.summary,
        plan.objective,
        plan.steps.len(),
        risks,
        verify_commands
    )
}

fn normalize_model_list(models: Vec<String>, current_model: &str) -> Vec<String> {
    let mut list = Vec::new();
    let current = current_model.trim();
    if !current.is_empty() {
        list.push(current.to_string());
    }

    for model in models {
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        if list.iter().any(|item| item == model) {
            continue;
        }
        list.push(model.to_string());
    }
    list
}
