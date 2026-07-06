use std::sync::Arc;

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

/// mcp-lock.json 中单个 MCP 依赖包的引用计数记录。
///
/// 语义：聚合所有已安装 skill 的 `requires.mcp` 声明，按 `package[@version]` 分组，
/// 记录被多少 skill 引用。不再记录安装路径与时间（旧字段的兼容读取见 serde default）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(in crate::app_state) struct McpDependencyLockRecord {
    /// 引用该依赖的 skill 数量。
    pub(in crate::app_state) ref_count: usize,
}

#[derive(Debug)]
pub struct AppPaths {
    pub app_storage_path: PathBuf,
    pub skills_config_path: PathBuf,
    pub mcp_config_path: PathBuf,
    pub mcp_capability_cache_path: PathBuf,
    pub sessions_dir_path: PathBuf,
}

#[derive(Debug)]
pub struct AppServices {
    pub mcp_service: AppMcpService,
    pub repository: AppRepository,
    pub runtime: RuntimeEngine,
    pub turn_service: AppTurnService,
    pub skill_registry: Arc<crate::skill::SkillRegistry>,
}
