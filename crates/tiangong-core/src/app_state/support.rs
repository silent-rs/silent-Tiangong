use super::*;

#[derive(Debug, Serialize, Deserialize, Default)]
pub(in crate::app_state) struct PersistedAppState {
    #[serde(default)]
    pub(in crate::app_state) active_session_id: String,
    #[serde(default)]
    pub(in crate::app_state) workspace_dir: String,
    #[serde(default)]
    pub(in crate::app_state) model_list: Vec<String>,
    #[serde(default)]
    pub(in crate::app_state) agent_config: Option<AgentConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::app_state) struct LegacyPersistedState {
    pub(in crate::app_state) sessions: Vec<Session>,
    pub(in crate::app_state) active_session_id: String,
    #[serde(default)]
    pub(in crate::app_state) model_config: Option<ModelProviderConfig>,
    #[serde(default)]
    pub(in crate::app_state) model_list: Vec<String>,
}

#[derive(Debug)]
pub(in crate::app_state) struct LoadedState {
    pub(in crate::app_state) sessions: Vec<Session>,
    pub(in crate::app_state) active_session_id: String,
    pub(in crate::app_state) workspace_dir: String,
    pub(in crate::app_state) model_list: Vec<String>,
    pub(in crate::app_state) agent_config: Option<AgentConfig>,
}

pub use tiangong_types::StreamEvent;

#[derive(Debug)]
pub struct AppPaths {
    pub app_storage_path: PathBuf,
    pub sessions_dir_path: PathBuf,
}

#[derive(Debug)]
pub struct AppServices {
    pub repository: AppRepository,
    pub runtime: RuntimeEngine,
    pub turn_service: AppTurnService,
}
