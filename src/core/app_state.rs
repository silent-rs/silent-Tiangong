use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::core::agent_config::AgentConfig;
use crate::core::model::{ModelProviderConfig, ModelStreamChunk, SingleProviderClient};
use crate::core::planner::{PlanStepStatus, TaskPlan};
use crate::core::runtime::{
    LlmOutputRecord, RunSnapshot, RunStatus, RuntimeEngine, TurnExecution, VerifyExecutionRecord,
};
use crate::core::session::{Message, MessageRole, Session, SessionTaskPlan, now_text};
use crate::core::tool::{ToolExecutionRecord, ToolResult};

const DEFAULT_SESSION_TITLE: &str = "默认会话";
const DEFAULT_CONTEXT_LIMIT: usize = 16;

#[derive(Debug, Serialize, Deserialize, Default)]
struct PersistedAppState {
    #[serde(default)]
    active_session_id: String,
    #[serde(default)]
    model_config: Option<ModelProviderConfig>,
    #[serde(default)]
    model_list: Vec<String>,
    #[serde(default)]
    agent_config: Option<AgentConfig>,
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
    agent_config: Option<AgentConfig>,
}

#[derive(Debug)]
enum TurnEvent {
    PlanReady(TaskPlan),
    LlmOutput(LlmOutputRecord),
    ToolExecution(ToolResult),
    PlanExecutionSummary(String),
    Chunk(ModelStreamChunk),
    Completed(Box<TurnExecution>),
    Failed(String),
}

#[derive(Debug)]
struct PendingTurn {
    session_id: String,
    task_id: String,
    assistant_message_id: Option<String>,
    started_at: Instant,
    rx: Receiver<TurnEvent>,
}

#[derive(Debug)]
pub struct TiangongState {
    sessions: Vec<Session>,
    active_session_id: String,
    model_config: ModelProviderConfig,
    agent_config: AgentConfig,
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
        let default_agent_config = AgentConfig::default();
        let runtime = RuntimeEngine::new(
            SingleProviderClient::new(default_model_config.clone()),
            DEFAULT_CONTEXT_LIMIT,
            default_agent_config.clone(),
        );

        let mut state = Self {
            sessions: Vec::new(),
            active_session_id: String::new(),
            model_config: default_model_config.clone(),
            agent_config: default_agent_config,
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

        let recovered_count = state.recover_interrupted_tasks();
        state.run.summary = format!("模型供应商：{}", state.runtime.provider_label());
        if recovered_count > 0 {
            state.run.status = RunStatus::Failed;
            state.run.summary = format!("已恢复 {recovered_count} 个中断任务（标记为失败）");
            state.run.last_result = Some("recovered_interrupted_tasks".to_string());
            state.run.last_error = Some("存在未完成任务，已在启动时恢复为失败".to_string());
            let _ = state.persist_to_disk();
        }

        if let Ok(true) = state.try_auto_resume_unfinished_plan_for_active_session() {
            state.run.summary = "检测到未完成 plan，已在启动时自动继续执行".to_string();
            state.run.last_result = Some("auto_resumed_unfinished_plan_on_startup".to_string());
            state.run.last_error = None;
        }
        state.run.updated_at = now_text();

        state
    }

    fn apply_loaded_state(&mut self, loaded: LoadedState) {
        self.sessions = loaded.sessions;
        self.active_session_id = loaded.active_session_id;
        self.settings_model_list = loaded.model_list;
        if let Some(agent_config) = loaded.agent_config {
            self.agent_config = agent_config;
        }
        if let Some(model_config) = loaded.model_config {
            self.model_config = model_config;
            self.runtime = RuntimeEngine::new(
                SingleProviderClient::new(self.model_config.clone()),
                DEFAULT_CONTEXT_LIMIT,
                self.agent_config.clone(),
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

    pub fn active_task_plans(&self) -> Vec<SessionTaskPlan> {
        let Some(session) = self.active_session() else {
            return Vec::new();
        };
        session.task_plans.to_vec()
    }

    pub fn has_pending_turn(&self) -> bool {
        self.pending_turn.is_some()
    }

    pub fn report_run_failed(&mut self, summary: impl Into<String>, error: impl Into<String>) {
        self.run = RunSnapshot {
            status: RunStatus::Failed,
            summary: summary.into(),
            last_session_id: self.run.last_session_id.clone(),
            last_task_id: self.run.last_task_id.clone(),
            last_duration_ms: self.run.last_duration_ms,
            last_result: self.run.last_result.clone(),
            last_plan: self.run.last_plan.clone(),
            last_tool_result: self.run.last_tool_result.clone(),
            last_error: Some(error.into()),
            last_usage: self.run.last_usage.clone(),
            updated_at: now_text(),
        };
    }

    pub fn report_run_idle(&mut self, summary: impl Into<String>) {
        self.run = RunSnapshot {
            status: RunStatus::Idle,
            summary: summary.into(),
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
    }

    pub fn cancel_pending_turn(&mut self) -> Result<bool> {
        let Some(pending) = self.pending_turn.take() else {
            return Ok(false);
        };

        let duration_ms = elapsed_ms_u64(pending.started_at.elapsed().as_millis());
        let mut cancelled_summary = "执行已取消".to_string();
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == pending.session_id)
        {
            if let Some(assistant_message_id) = pending.assistant_message_id.as_deref()
                && let Some(position) = session.messages.iter().position(|msg| {
                    msg.id == assistant_message_id
                        && msg.content.trim().is_empty()
                        && msg.reasoning_content.trim().is_empty()
                })
            {
                session.messages.remove(position);
                session.updated_at = now_text();
            }
            session.append_message(MessageRole::System, "执行已取消：用户主动中断");
            session.fail_task(
                &pending.task_id,
                "执行已取消",
                Some("cancelled_by_user".to_string()),
                duration_ms,
            );
            cancelled_summary = format!("执行已取消（会话：{}）", session.title);
        }

        self.run = RunSnapshot {
            status: RunStatus::Failed,
            summary: "执行已取消".to_string(),
            last_session_id: Some(pending.session_id.clone()),
            last_task_id: Some(pending.task_id),
            last_duration_ms: Some(duration_ms),
            last_result: Some("failed".to_string()),
            last_plan: self.run.last_plan.clone(),
            last_tool_result: self.run.last_tool_result.clone(),
            last_error: Some("cancelled_by_user".to_string()),
            last_usage: None,
            updated_at: now_text(),
        };

        self.persist_session_and_app(&pending.session_id)?;
        self.run.summary = cancelled_summary;

        Ok(true)
    }

    pub fn delete_pending_task_plan(&mut self, pending_index_1_based: usize) -> Result<bool> {
        if pending_index_1_based == 0 {
            return Err(anyhow!("删除索引必须从 1 开始"));
        }

        let active_id = self.active_session_id.clone();
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
        else {
            return Err(anyhow!("当前会话不存在"));
        };

        let removed = session.delete_pending_task_plan(pending_index_1_based - 1);
        if removed {
            self.persist_session_and_app(&active_id)?;
        }
        Ok(removed)
    }

    pub fn move_pending_task_plan(
        &mut self,
        from_index_1_based: usize,
        to_index_1_based: usize,
    ) -> Result<bool> {
        if from_index_1_based == 0 || to_index_1_based == 0 {
            return Err(anyhow!("调序索引必须从 1 开始"));
        }

        let active_id = self.active_session_id.clone();
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
        else {
            return Err(anyhow!("当前会话不存在"));
        };

        let moved = session.move_pending_task_plan(from_index_1_based - 1, to_index_1_based - 1);
        if moved {
            self.persist_session_and_app(&active_id)?;
        }
        Ok(moved)
    }

    pub fn poll_pending_turn(&mut self) {
        let mut should_clear = false;
        let mut disconnected = false;

        while let Some(event) = self.try_recv_turn_event(&mut disconnected) {
            match event {
                TurnEvent::PlanReady(plan) => {
                    self.mark_pending_turn_executing(&plan);
                }
                TurnEvent::LlmOutput(output) => {
                    self.append_pending_turn_llm_output(&output);
                }
                TurnEvent::ToolExecution(result) => {
                    self.append_pending_turn_tool_execution(&result);
                }
                TurnEvent::PlanExecutionSummary(summary) => {
                    self.append_pending_turn_plan_execution_summary(summary.as_str());
                }
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
            let _ = self.try_auto_resume_unfinished_plan_for_active_session();
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

    pub fn agent_config_summary(&self) -> String {
        format!(
            "skills.enabled={}, skills.max_matches={}, skills.dirs={}, mcp.enabled={}, mcp.timeout_ms={}, mcp.servers={}",
            self.agent_config.skills.enabled,
            self.agent_config.skills.max_matches,
            self.agent_config.skills.dirs.len(),
            self.agent_config.mcp.enabled,
            self.agent_config.mcp.timeout_ms,
            self.agent_config.mcp.servers.len()
        )
    }

    pub fn validate_agent_config(&self) -> Result<()> {
        validate_agent_config(&self.agent_config)
    }

    pub fn update_agent_config_entry(&mut self, key: &str, value: &str) -> Result<String> {
        let key = key.trim();
        let value = value.trim();

        if key.is_empty() {
            return Err(anyhow!("配置键不能为空"));
        }

        let updated_value = match key {
            "skills.enabled" => {
                let parsed = parse_bool(value)?;
                self.agent_config.skills.enabled = parsed;
                parsed.to_string()
            }
            "skills.max_matches" => {
                let parsed = value
                    .parse::<usize>()
                    .with_context(|| format!("配置值无效，要求正整数：{value}"))?;
                if parsed == 0 {
                    return Err(anyhow!("skills.max_matches 必须大于 0"));
                }
                self.agent_config.skills.max_matches = parsed;
                parsed.to_string()
            }
            "skills.dirs" => {
                let parsed = parse_list_value(value);
                self.agent_config.skills.dirs = parsed.clone();
                if parsed.is_empty() {
                    "(empty)".to_string()
                } else {
                    parsed.join(",")
                }
            }
            "mcp.enabled" => {
                let parsed = parse_bool(value)?;
                self.agent_config.mcp.enabled = parsed;
                parsed.to_string()
            }
            "mcp.timeout_ms" => {
                let parsed = value
                    .parse::<u64>()
                    .with_context(|| format!("配置值无效，要求正整数：{value}"))?;
                if parsed == 0 {
                    return Err(anyhow!("mcp.timeout_ms 必须大于 0"));
                }
                self.agent_config.mcp.timeout_ms = parsed;
                parsed.to_string()
            }
            _ => {
                return Err(anyhow!(
                    "不支持的配置键：{key}。支持：skills.enabled、skills.max_matches、skills.dirs、mcp.enabled、mcp.timeout_ms"
                ));
            }
        };

        validate_agent_config(&self.agent_config)?;

        self.runtime = RuntimeEngine::new(
            SingleProviderClient::new(self.model_config.clone()),
            DEFAULT_CONTEXT_LIMIT,
            self.agent_config.clone(),
        );
        self.persist_app_only()?;

        Ok(format!("配置已更新：{key}={updated_value}"))
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
            self.agent_config.clone(),
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
            self.agent_config.clone(),
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
        let input = self.input_draft.trim().to_string();
        let started = self.start_turn_with_input(input)?;
        if started {
            self.input_draft.clear();
        }
        Ok(())
    }

    fn start_turn_with_input(&mut self, input: String) -> Result<bool> {
        if self.pending_turn.is_some() {
            return Ok(false);
        }
        if input.trim().is_empty() {
            return Ok(false);
        }
        let active_idx = self.ensure_active_session_index();
        let session_id = self.sessions[active_idx].id.clone();
        let task_id = new_scru128_string();
        self.sessions[active_idx].append_message(MessageRole::User, input.clone());
        let user_message_id = self.sessions[active_idx]
            .messages
            .last()
            .map(|msg| msg.id.clone())
            .ok_or_else(|| anyhow!("创建用户消息失败"))?;
        self.sessions[active_idx].start_task(
            task_id.clone(),
            user_message_id,
            String::new(),
            input.clone(),
        );

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

        self.persist_session_and_app(&session_id)?;

        let runtime = self.runtime.clone();
        let session_snapshot = self.sessions[active_idx].clone();
        let worker_input = input.clone();
        let (tx, rx) = mpsc::channel::<TurnEvent>();

        thread::spawn(move || {
            let chunk_tx = tx.clone();
            let plan_tx = tx.clone();
            let llm_tx = tx.clone();
            let tool_tx = tx.clone();
            let plan_summary_tx = tx.clone();
            let result = runtime.execute_turn_with_streaming(
                &session_snapshot,
                &worker_input,
                |plan| {
                    let _ = plan_tx.send(TurnEvent::PlanReady(plan.clone()));
                },
                |delta| {
                    let _ = chunk_tx.send(TurnEvent::Chunk(delta.clone()));
                },
                |output| {
                    let _ = llm_tx.send(TurnEvent::LlmOutput(output.clone()));
                },
                |tool_result| {
                    let _ = tool_tx.send(TurnEvent::ToolExecution(tool_result.clone()));
                },
                |summary| {
                    let _ =
                        plan_summary_tx.send(TurnEvent::PlanExecutionSummary(summary.to_string()));
                },
            );

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
            assistant_message_id: None,
            started_at: Instant::now(),
            rx,
        });

        Ok(true)
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

    fn apply_assistant_delta(&mut self, delta: &ModelStreamChunk) {
        if delta.content.is_empty() && delta.reasoning_content.is_empty() {
            return;
        }

        let Some((session_id, assistant_message_id)) = self.ensure_pending_turn_assistant_message()
        else {
            return;
        };

        if let Some(message) = self.find_message_mut(&session_id, &assistant_message_id) {
            message.content.push_str(&delta.content);
            message.reasoning_content.push_str(&delta.reasoning_content);
        }
    }

    fn append_pending_turn_llm_output(&mut self, output: &LlmOutputRecord) {
        let Some(session_id) = self
            .pending_turn
            .as_ref()
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };

        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.append_message(MessageRole::System, format_llm_output_message(output));
            let _ = self.persist_session_and_app(&session_id);
        }
    }

    fn append_pending_turn_tool_execution(&mut self, result: &ToolResult) {
        let Some(session_id) = self
            .pending_turn
            .as_ref()
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };

        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.append_message(MessageRole::System, format_tool_trace_message(result));
            let _ = self.persist_session_and_app(&session_id);
        }
    }

    fn append_pending_turn_plan_execution_summary(&mut self, summary: &str) {
        let Some(session_id) = self
            .pending_turn
            .as_ref()
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };
        let summary = summary.trim();
        if summary.is_empty() {
            return;
        }

        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.append_message(MessageRole::System, format!("Plan 执行总结\n{summary}"));
            let _ = self.persist_session_and_app(&session_id);
        }
    }

    fn ensure_pending_turn_assistant_message(&mut self) -> Option<(String, String)> {
        let (session_id, task_id, existing_message_id) =
            self.pending_turn.as_ref().map(|pending| {
                (
                    pending.session_id.clone(),
                    pending.task_id.clone(),
                    pending.assistant_message_id.clone(),
                )
            })?;

        if let Some(message_id) = existing_message_id {
            return Some((session_id, message_id));
        }

        let assistant_message_id = {
            let session = self
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)?;
            session.append_message(MessageRole::Assistant, String::new());
            session.messages.last().map(|msg| msg.id.clone())?
        };

        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.bind_task_assistant_message_id(&task_id, assistant_message_id.clone());
        }
        if let Some(pending) = self.pending_turn.as_mut()
            && pending.session_id == session_id
            && pending.task_id == task_id
        {
            pending.assistant_message_id = Some(assistant_message_id.clone());
        }

        Some((session_id, assistant_message_id))
    }

    fn mark_pending_turn_executing(&mut self, plan: &TaskPlan) {
        let Some((session_id, task_id)) = self
            .pending_turn
            .as_ref()
            .map(|pending| (pending.session_id.clone(), pending.task_id.clone()))
        else {
            return;
        };

        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.mark_task_executing(&task_id, Some(format_plan_snapshot(plan)));
            session.sync_task_plans(&task_id, &plan.plans);
        }

        self.run = RunSnapshot {
            status: RunStatus::Executing,
            summary: "正在流式调用模型".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id),
            last_duration_ms: None,
            last_result: None,
            last_plan: Some(format_plan_snapshot(plan)),
            last_tool_result: None,
            last_error: None,
            last_usage: None,
            updated_at: now_text(),
        };

        let _ = self.persist_session_and_app(&session_id);
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

        let mut final_assistant_message_id = assistant_message_id;
        let mut updated_existing_message = false;
        if let Some(message_id) = final_assistant_message_id.as_deref()
            && let Some(message) = self.find_message_mut(&session_id, message_id)
        {
            message.content = exec.assistant_message.clone();
            message.reasoning_content = exec.assistant_reasoning_content.clone();
            updated_existing_message = true;
        }
        if (final_assistant_message_id.is_none() || !updated_existing_message)
            && let Some(session) = self
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
        {
            session.append_message_with_reasoning(
                MessageRole::Assistant,
                exec.assistant_message.clone(),
                exec.assistant_reasoning_content.clone(),
            );
            final_assistant_message_id = session.messages.last().map(|message| message.id.clone());
        }

        let base_result = format!(
            "success; output_mode={}; chunks={}",
            exec.output_mode, exec.output_chunk_count
        );
        let duration_ms = elapsed_ms_u64(started_at.elapsed().as_millis());
        let plan_snapshot = format_plan_snapshot(&exec.plan);
        let turn_conclusion = build_turn_conclusion(&exec);
        let tool_result_text = merge_tool_result_text(
            exec.tool_result_summary,
            exec.tool_execution.as_ref(),
            &exec.verify_records,
        );
        let result_with_workspace = match (
            workspace_change_overview(),
            summarize_verify_for_result(&exec.verify_records),
        ) {
            (Some(overview), Some(verify)) => {
                format!("{base_result}; {overview}; {verify}; {turn_conclusion}")
            }
            (Some(overview), None) => format!("{base_result}; {overview}; {turn_conclusion}"),
            (None, Some(verify)) => format!("{base_result}; {verify}; {turn_conclusion}"),
            (None, None) => format!("{base_result}; {turn_conclusion}"),
        };
        let completion_tool_result = tool_result_text.clone();
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if let Some(message_id) = final_assistant_message_id.clone() {
                session.bind_task_assistant_message_id(&task_id, message_id);
            }
            session.sync_task_plans(&task_id, &exec.plan.plans);
            session.complete_task(
                &task_id,
                Some(plan_snapshot.clone()),
                completion_tool_result,
                duration_ms,
            );
        }

        self.run = RunSnapshot {
            status: RunStatus::Completed,
            summary: "执行完成".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id),
            last_duration_ms: Some(duration_ms),
            last_result: Some(result_with_workspace),
            last_plan: Some(plan_snapshot),
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
        let duration_ms = elapsed_ms_u64(started_at.elapsed().as_millis());

        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if let Some(assistant_message_id) = assistant_message_id.as_deref()
                && let Some(position) = session.messages.iter().position(|msg| {
                    msg.id == assistant_message_id
                        && msg.content.trim().is_empty()
                        && msg.reasoning_content.trim().is_empty()
                })
            {
                session.messages.remove(position);
                session.updated_at = now_text();
            }
            session.append_message(MessageRole::System, err_msg);
            session.fail_task(&task_id, "执行失败", Some(err_msg.to_string()), duration_ms);
        }

        self.run = RunSnapshot {
            status: RunStatus::Failed,
            summary: "执行失败".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id),
            last_duration_ms: Some(duration_ms),
            last_result: Some("failed".to_string()),
            last_plan: self.run.last_plan.clone(),
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
            model_config: Some(self.model_config.clone()),
            model_list: self.settings_model_list.clone(),
            agent_config: Some(self.agent_config.clone()),
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

    fn try_auto_resume_unfinished_plan_for_active_session(&mut self) -> Result<bool> {
        if self.pending_turn.is_some() {
            return Ok(false);
        }

        let active_id = self.active_session_id.clone();
        let Some(session) = self.sessions.iter().find(|session| session.id == active_id) else {
            return Ok(false);
        };
        let Some(last_task) = session.task_records.last() else {
            return Ok(false);
        };
        let has_pending_plans = session.task_plans.iter().any(|plan| {
            plan.task_id == last_task.task_id && plan.status == PlanStepStatus::Pending
        });
        if !has_pending_plans {
            return Ok(false);
        }

        let resume_input = last_task.user_input.trim().to_string();
        if resume_input.is_empty() {
            return Ok(false);
        }

        self.start_turn_with_input(resume_input)
    }

    fn recover_interrupted_tasks(&mut self) -> usize {
        let mut recovered = 0usize;
        for session in &mut self.sessions {
            let recovered_in_session = session.recover_interrupted_tasks();
            if recovered_in_session > 0 {
                recovered += recovered_in_session;
                session.append_message(
                    MessageRole::System,
                    format!(
                        "检测到 {} 个未完成任务，已在启动时恢复为失败状态",
                        recovered_in_session
                    ),
                );
            }
        }
        recovered
    }

    fn load_from_disk(&self) -> Result<Option<LoadedState>> {
        let session_ids = self.list_session_ids_from_dir()?;
        if !self.app_storage_path.exists() {
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
                agent_config: None,
            }));
        }

        let content = fs::read_to_string(&self.app_storage_path)
            .with_context(|| format!("读取应用存储失败：{}", self.app_storage_path.display()))?;
        let persisted: PersistedAppState =
            serde_json::from_str(&content).context("解析应用存储失败")?;

        let mut sessions = Vec::new();
        for session_id in &session_ids {
            if let Some(session) = self.load_session_from_disk(session_id)? {
                sessions.push(session);
            }
        }
        let active_session_id = if session_ids
            .iter()
            .any(|session_id| session_id == &persisted.active_session_id)
        {
            persisted.active_session_id
        } else {
            session_ids.first().cloned().unwrap_or_default()
        };

        Ok(Some(LoadedState {
            sessions,
            active_session_id,
            model_config: persisted.model_config,
            model_list: persisted.model_list,
            agent_config: persisted.agent_config,
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
            agent_config: None,
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
            model_config: Some(self.model_config.clone()),
            model_list: self.settings_model_list.clone(),
            agent_config: Some(self.agent_config.clone()),
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
    user_storage_root()
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

fn user_storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }

    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }

    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buf = PathBuf::from(drive);
            buf.push(path);
            Some(buf)
        }
        _ => None,
    }
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
    verify_records: &[VerifyExecutionRecord],
) -> Option<String> {
    let base_text = match (base, record) {
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
    };

    let verify_text = summarize_verify_for_result(verify_records);
    match (base_text, verify_text) {
        (Some(base), Some(verify)) => Some(format!("{base} | {verify}")),
        (Some(base), None) => Some(base),
        (None, Some(verify)) => Some(verify),
        (None, None) => None,
    }
}

fn format_plan_snapshot(plan: &TaskPlan) -> String {
    let risks = if plan.risks.is_empty() {
        "无".to_string()
    } else {
        plan.risks.join("；")
    };
    let plan_count = plan.plans.len();
    let step_count = plan
        .plans
        .iter()
        .map(|item| item.execution_steps.len())
        .sum::<usize>();
    let skill_hints = if plan.skill_hints.is_empty() {
        "无".to_string()
    } else {
        plan.skill_hints.join("；")
    };
    let mcp_hints = if plan.mcp_hints.is_empty() {
        "无".to_string()
    } else {
        plan.mcp_hints.join("；")
    };
    let revisions = if plan.revisions.is_empty() {
        "无".to_string()
    } else {
        plan.revisions
            .iter()
            .enumerate()
            .map(|(idx, revision)| {
                format!(
                    "{}. [{}] {} => {}",
                    idx + 1,
                    revision.phase,
                    revision.reason,
                    revision.summary_after_revision
                )
            })
            .collect::<Vec<_>>()
            .join("；")
    };

    format!(
        "{}\n目标：{}\n事项数：{}\n执行步骤数：{}\n风险：{}\nSkills：{}\nMCP：{}\n计划修正：{}",
        plan.summary,
        plan.objective,
        plan_count,
        step_count,
        risks,
        skill_hints,
        mcp_hints,
        revisions
    )
}

fn format_llm_output_message(output: &LlmOutputRecord) -> String {
    let mut lines = vec![format!("LLM 输出 [{}]", output.stage)];
    if !output.tool_calls.is_empty() {
        lines.push(format!("tool_calls: {}", output.tool_calls.join(", ")));
    }
    if !output.reasoning_content.trim().is_empty() {
        lines.push(format!("reasoning:\n{}", output.reasoning_content.trim()));
    }
    if !output.content.trim().is_empty() {
        lines.push(format!("content:\n{}", output.content.trim()));
    }
    lines.join("\n")
}

fn format_tool_trace_message(result: &ToolResult) -> String {
    let Some(record) = result.execution.as_ref() else {
        let mut lines = vec!["工具执行 [unknown]".to_string()];
        lines.push(format!("summary: {}", result.summary));
        let stdout_text = clip_tool_output_lines(result.stdout.as_str(), 5);
        let stderr_text = clip_tool_output_lines(result.stderr.as_str(), 5);
        if !stdout_text.trim().is_empty() {
            lines.push("stdout:".to_string());
            lines.push("```text".to_string());
            lines.push(stdout_text);
            lines.push("```".to_string());
        }
        if !stderr_text.trim().is_empty() {
            lines.push("stderr:".to_string());
            lines.push("```text".to_string());
            lines.push(stderr_text);
            lines.push("```".to_string());
        }
        return lines.join("\n");
    };

    let mut lines = vec![format!("工具执行 [{}]", record.tool_name)];
    if let Some(command) = format_tool_command(record) {
        lines.push(format!("命令: {command}"));
    }
    lines.push(format!(
        "ok={} exit_code={} duration_ms={}",
        result.ok, result.exit_code, record.duration_ms
    ));
    lines.push(format!("summary: {}", result.summary));
    let stdout_text = clip_tool_output_lines(result.stdout.as_str(), 5);
    let stderr_text = clip_tool_output_lines(result.stderr.as_str(), 5);
    if !stdout_text.trim().is_empty() {
        lines.push("stdout:".to_string());
        lines.push("```text".to_string());
        lines.push(stdout_text);
        lines.push("```".to_string());
    }
    if !stderr_text.trim().is_empty() {
        lines.push("stderr:".to_string());
        lines.push("```text".to_string());
        lines.push(stderr_text);
        lines.push("```".to_string());
    }
    lines.join("\n")
}

fn clip_tool_output_lines(text: &str, max_lines: usize) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() <= max_lines {
        return text.to_string();
    }
    let kept = lines
        .iter()
        .take(max_lines)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    format!("{kept}\n...(省略 {} 行)", lines.len() - max_lines)
}

fn format_tool_command(record: &ToolExecutionRecord) -> Option<String> {
    let args = record
        .args
        .iter()
        .filter(|arg| !arg.starts_with("__tiangong_cwd="))
        .cloned()
        .collect::<Vec<_>>();
    if args.is_empty() {
        return None;
    }

    if record.tool_name == "run_command" {
        if args.first().map(String::as_str) == Some("__tiangong_shell__") {
            let script = args.get(1).cloned().unwrap_or_default();
            let shell = args.get(2).cloned().unwrap_or_else(|| "auto".to_string());
            return Some(format!("shell={shell} script={script}"));
        }
        let cmd = args.first().cloned().unwrap_or_default();
        let rest = args.into_iter().skip(1).collect::<Vec<_>>();
        if rest.is_empty() {
            return Some(cmd);
        }
        return Some(format!("{cmd} {}", rest.join(" ")));
    }

    if record.tool_name == "write_file" {
        let path = args.first().cloned().unwrap_or_default();
        let content_bytes = args.get(1).map(|content| content.len()).unwrap_or(0usize);
        let append = args.get(2).cloned().unwrap_or_else(|| "false".to_string());
        return Some(format!(
            "path={} content=...({content_bytes} bytes) append={append}",
            single_line_ellipsis(path.as_str(), 120)
        ));
    }

    Some(args.join(" "))
}

fn single_line_ellipsis(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return String::new();
    }
    let mut chars = normalized.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn workspace_change_overview() -> Option<String> {
    let status_output = Command::new("git")
        .arg("status")
        .arg("--short")
        .output()
        .ok()?;
    if !status_output.status.success() {
        return None;
    }
    let status_text = String::from_utf8_lossy(&status_output.stdout);
    let mut files = Vec::new();
    for line in status_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = trimmed
            .split_whitespace()
            .last()
            .unwrap_or(trimmed)
            .to_string();
        files.push(path);
    }
    if files.is_empty() {
        return Some("changed_files=0".to_string());
    }

    let preview_limit = 6usize;
    let preview = files
        .iter()
        .take(preview_limit)
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    let extra = files.len().saturating_sub(preview_limit);
    let file_part = if extra == 0 {
        format!("changed_files={},files={preview}", files.len())
    } else {
        format!(
            "changed_files={},files={}...(+{})",
            files.len(),
            preview,
            extra
        )
    };

    let diff_output = Command::new("git")
        .arg("diff")
        .arg("--stat")
        .output()
        .ok()?;
    if !diff_output.status.success() {
        return Some(file_part);
    }
    let diff_text = String::from_utf8_lossy(&diff_output.stdout);
    let mut stat_summary = String::new();
    for line in diff_text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.contains("files changed")
            || trimmed.contains("file changed")
            || trimmed.contains("insertions")
            || trimmed.contains("deletions")
        {
            stat_summary = trimmed.to_string();
            break;
        }
    }

    if stat_summary.is_empty() {
        Some(file_part)
    } else {
        Some(format!("{file_part}; diff={stat_summary}"))
    }
}

fn build_turn_conclusion(exec: &TurnExecution) -> String {
    let mut completed = vec!["计划生成".to_string(), "模型响应".to_string()];
    if let Some(tool_execution) = &exec.tool_execution {
        completed.push(format!("工具执行({})", tool_execution.tool_name));
    }
    let pending_plans = exec
        .plan
        .plans
        .iter()
        .filter(|item| item.status == PlanStepStatus::Pending)
        .map(|item| item.name.clone())
        .collect::<Vec<_>>();
    let failed_plans = exec
        .plan
        .plans
        .iter()
        .filter(|item| item.status == PlanStepStatus::Failed)
        .map(|item| {
            let summary = item.execution_summary.clone().unwrap_or_default();
            if summary.is_empty() {
                item.name.clone()
            } else {
                format!("{}({})", item.name, summary.replace('\n', " | "))
            }
        })
        .collect::<Vec<_>>();
    let ignored_step_count = exec
        .plan
        .plans
        .iter()
        .flat_map(|item| item.execution_steps.iter())
        .filter(|step| step.status == PlanStepStatus::Ignored)
        .count();
    let failed_verify = exec
        .verify_records
        .iter()
        .filter(|record| !record.ok)
        .collect::<Vec<_>>();

    if pending_plans.is_empty() && failed_plans.is_empty() {
        completed.push("plan事项执行".to_string());
    }

    let pending = if !pending_plans.is_empty() {
        format!("待完成 plan：{}", pending_plans.join("；"))
    } else if !failed_plans.is_empty() {
        format!(
            "plan执行存在失败：{}；忽略步骤数={ignored_step_count}",
            failed_plans.join("；")
        )
    } else if exec.verify_records.is_empty() {
        "人工复核输出结果".to_string()
    } else if failed_verify.is_empty() {
        completed.push("验证执行".to_string());
        "无".to_string()
    } else {
        let hints = failed_verify
            .iter()
            .map(|record| format!("{} => {}", record.command, record.summary))
            .collect::<Vec<_>>();
        format!("修复验证失败：{}", hints.join("；"))
    };

    let risks = if exec.plan.risks.is_empty() {
        "无".to_string()
    } else {
        exec.plan.risks.join("；")
    };

    format!(
        "结论=完成:{} | 未完成:{} | 风险:{}",
        completed.join("、"),
        pending,
        risks
    )
}

fn summarize_verify_for_result(verify_records: &[VerifyExecutionRecord]) -> Option<String> {
    if verify_records.is_empty() {
        return None;
    }

    let passed = verify_records.iter().filter(|record| record.ok).count();
    let failed = verify_records.len().saturating_sub(passed);
    let slowest_ms = verify_records
        .iter()
        .map(|record| record.duration_ms)
        .max()
        .unwrap_or(0);
    let output_bytes = verify_records
        .iter()
        .map(|record| record.stdout.len() + record.stderr.len())
        .sum::<usize>();
    let first_failure = verify_records
        .iter()
        .find(|record| !record.ok)
        .map(|record| {
            let detail = first_non_empty_line(&record.stderr)
                .or_else(|| first_non_empty_line(&record.stdout))
                .unwrap_or_else(|| "无".to_string());
            format!(
                "; first_failure={} (exit_code={}) detail={}",
                record.command, record.exit_code, detail
            )
        })
        .unwrap_or_default();
    Some(format!(
        "verify_passed={}/{}; verify_failed={}; verify_slowest_ms={}; verify_output_bytes={}{}",
        passed,
        verify_records.len(),
        failed,
        slowest_ms,
        output_bytes,
        first_failure
    ))
}

fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
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

fn validate_agent_config(config: &AgentConfig) -> Result<()> {
    if config.skills.max_matches == 0 {
        return Err(anyhow!("skills.max_matches 必须大于 0"));
    }
    if config.mcp.timeout_ms == 0 {
        return Err(anyhow!("mcp.timeout_ms 必须大于 0"));
    }
    if let Some(server) = config
        .mcp
        .servers
        .iter()
        .find(|server| server.name.trim().is_empty())
    {
        return Err(anyhow!("mcp.servers 包含空名称配置：{:?}", server));
    }
    Ok(())
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(anyhow!("布尔值无效：{raw}（可用 true/false）")),
    }
}

fn parse_list_value(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "[]" || trimmed == "-" {
        return Vec::new();
    }

    trimmed
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
}
