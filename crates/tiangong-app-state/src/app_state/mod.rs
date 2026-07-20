use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use tiangong_core::agent_config::AgentConfig;
use tiangong_core::runtime::RunSnapshot;
use tiangong_core::session::atomic_replace_file;

mod store;
#[cfg(test)]
mod tests;

pub use self::store::{PendingTurnStub, SessionInputDraft};

pub use tiangong_core_manager::CoreManager;
pub use tiangong_types::StreamEvent;

const DEFAULT_SESSION_TITLE: &str = "默认会话";

/// app.json 序列化结构。
#[derive(Debug, Serialize, Deserialize, Default)]
struct PersistedAppState {
    #[serde(default)]
    active_session_id: String,
    #[serde(default)]
    workspace_dir: String,
    #[serde(default)]
    model_list: Vec<String>,
    #[serde(default)]
    agent_config: Option<AgentConfig>,
    #[serde(default)]
    input_drafts: HashMap<String, SessionInputDraft>,
}

/// 应用状态存储（issue #245：纯状态容器，不含业务逻辑）。
#[derive(Debug)]
pub struct TiangongState {
    pub active_session_id: String,
    pub workspace_dir: String,
    pub session_title_draft: String,
    pub input_drafts: HashMap<String, SessionInputDraft>,
    pub model_list: Vec<String>,
    pub agent_config: AgentConfig,
    pub run: RunSnapshot,
    pub pending_turns: HashMap<String, PendingTurnStub>,
    pub core_manager: std::sync::OnceLock<CoreManager>,
}

impl Default for TiangongState {
    fn default() -> Self {
        Self::load_or_default()
    }
}

impl TiangongState {
    pub fn install_core_manager(
        &self,
        config: tiangong_core::core_config::CoreConfigProvider,
        storage_root: impl Into<PathBuf>,
    ) -> &CoreManager {
        self.core_manager
            .get_or_init(|| CoreManager::new(config, storage_root))
    }

    pub fn core_manager(&self) -> &CoreManager {
        self.core_manager
            .get()
            .expect("CoreManager 尚未注入，需先调用 install_core_manager")
    }

    /// app.json 路径。
    fn app_json_path() -> PathBuf {
        tiangong_config::io::storage_root().join("app.json")
    }

    /// 持久化 app.json。
    pub fn persist(&self) -> Result<()> {
        let payload = PersistedAppState {
            active_session_id: self.active_session_id.clone(),
            workspace_dir: self.workspace_dir.clone(),
            model_list: self.model_list.clone(),
            agent_config: Some(self.agent_config.clone()),
            input_drafts: self
                .input_drafts
                .iter()
                .map(|(id, draft)| {
                    let mut d = draft.clone();
                    d.is_sending = false;
                    (id.clone(), d)
                })
                .collect(),
        };
        let content = serde_json::to_string_pretty(&payload).context("序列化应用存储失败")?;
        let path = Self::app_json_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        }
        atomic_replace_file(&path, content.as_bytes())
            .with_context(|| format!("写入应用存储失败：{}", path.display()))?;
        Ok(())
    }

    /// 从磁盘加载 app.json + 初始默认值。
    pub fn load_or_default() -> Self {
        let mut state = Self {
            active_session_id: String::new(),
            workspace_dir: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            session_title_draft: DEFAULT_SESSION_TITLE.to_string(),
            input_drafts: HashMap::new(),
            model_list: Vec::new(),
            agent_config: AgentConfig::default(),
            run: RunSnapshot::default(),
            pending_turns: HashMap::new(),
            core_manager: std::sync::OnceLock::new(),
        };

        let path = Self::app_json_path();
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(persisted) = serde_json::from_str::<PersistedAppState>(&content) {
                    state.active_session_id = persisted.active_session_id;
                    if !persisted.workspace_dir.trim().is_empty() {
                        state.workspace_dir = persisted.workspace_dir;
                    }
                    state.model_list = persisted.model_list;
                    state.input_drafts = persisted.input_drafts;
                    if let Some(ac) = persisted.agent_config {
                        state.agent_config = ac;
                    }
                }
            }
        }

        let _ = state.persist();
        state
    }
}
