use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use tiangong_core::agent_config::AgentConfig;
use tiangong_core::model::SingleProviderClient;
use tiangong_core::runtime::{RunSnapshot, RunStatus, RuntimeEngine};
use tiangong_core::session::{MessageRole, Session, SessionTaskPlan, now_text};

mod facade;
pub(crate) mod repository;
mod services;
mod store;
pub(crate) mod support;
#[cfg(test)]
mod tests;

// Private imports
use self::repository::{
    default_app_storage_path, default_sessions_dir_path, default_workspace_dir, init_storage_root,
    normalize_model_list, validate_agent_config,
};
use self::services::AppTurnService;
pub use self::support::StreamEvent;
use self::support::{LegacyPersistedState, LoadedState, PersistedAppState};

// Public re-exports for Tauri API
pub use self::repository::AppRepository;
pub use self::repository::storage_root;
pub use self::store::{
    AgentState, AppStore, PendingTurnStub, ProviderState, RuntimeState, SessionState,
};
pub use self::support::{AppPaths, AppServices};

const DEFAULT_SESSION_TITLE: &str = "默认会话";

#[derive(Debug)]
pub struct TiangongState {
    pub store: AppStore,
    pub services: AppServices,
}

impl Default for TiangongState {
    fn default() -> Self {
        Self::load_or_default()
    }
}

impl TiangongState {
    pub fn input_draft(&self) -> &str {
        &self.store.session.input_draft
    }

    pub fn run_snapshot(&self) -> &RunSnapshot {
        &self.store.runtime.run
    }

    pub fn agent_config(&self) -> &AgentConfig {
        &self.store.agent.agent_config
    }

    /// 获取当前活跃的 Worker 列表（已迁移到 TiangongCore 管理）
    pub fn list_active_workers(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    pub fn set_trust_mode(&mut self, mode: tiangong_core::permission::TrustMode) -> Result<()> {
        let active_id = self.store.session.active_session_id.clone();
        let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
        else {
            return Err(anyhow::anyhow!("当前会话不存在，无法设置信任模式"));
        };
        session.trust_mode = mode;
        // 兼容旧的状态读取；真实来源是当前会话。
        self.store.agent.agent_config.trust_mode = mode;
        self.services.runtime.permission_gate().set_trust_mode(mode);
        self.rebuild_runtime_from_current_config();
        self.persist_session_and_app(&active_id)
    }

    pub fn set_default_trust_mode(
        &mut self,
        mode: tiangong_core::permission::TrustMode,
    ) -> Result<()> {
        self.store.agent.agent_config.default_trust_mode = mode;
        self.persist_app_only()
    }

    pub fn set_custom_system_prompt(&mut self, prompt: String) -> Result<()> {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            // 清空：删除 custom-prompt.md 并清空旧字段
            tiangong_config::io::clear_custom_prompt()?;
            self.store.agent.agent_config.custom_system_prompt = String::new();
        } else {
            // 写入 custom-prompt.md 作为唯一事实来源，并清空 app.json 旧字段
            tiangong_config::io::save_custom_prompt(&prompt)?;
            self.store.agent.agent_config.custom_system_prompt = String::new();
        }
        self.rebuild_runtime_from_current_config();
        self.persist_app_only()
    }

    /// 获取自定义 Prompt 内容（已按 custom-prompt.md > 旧字段 > 空 优先级加载）。
    pub fn custom_system_prompt(&self) -> &str {
        &self.store.agent.agent_config.custom_system_prompt
    }

    /// 获取自定义 Prompt 独立存储路径（~/.tiangong/custom-prompt.md）。
    pub fn custom_prompt_path(&self) -> std::path::PathBuf {
        tiangong_config::io::custom_prompt_path()
    }

    pub fn set_reasoning_effort(&mut self, effort: String) -> Result<()> {
        self.store.agent.agent_config.reasoning_effort = effort.clone();
        let active_id = self.store.session.active_session_id.clone();
        if let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
        {
            session.reasoning_effort = Some(effort);
            self.persist_session_and_app(&active_id)
        } else {
            self.persist_app_only()
        }
    }
}
