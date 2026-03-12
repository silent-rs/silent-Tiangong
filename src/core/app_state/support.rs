use super::*;

#[derive(Debug, Serialize, Deserialize, Default)]
pub(in crate::core::app_state) struct PersistedAppState {
    #[serde(default)]
    pub(in crate::core::app_state) active_session_id: String,
    #[serde(default)]
    pub(in crate::core::app_state) model_config: Option<ModelProviderConfig>,
    #[serde(default)]
    pub(in crate::core::app_state) model_list: Vec<String>,
    #[serde(default)]
    pub(in crate::core::app_state) agent_config: Option<AgentConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::core::app_state) struct LegacyPersistedState {
    pub(in crate::core::app_state) sessions: Vec<Session>,
    pub(in crate::core::app_state) active_session_id: String,
    #[serde(default)]
    pub(in crate::core::app_state) model_config: Option<ModelProviderConfig>,
    #[serde(default)]
    pub(in crate::core::app_state) model_list: Vec<String>,
}

#[derive(Debug)]
pub(in crate::core::app_state) struct LoadedState {
    pub(in crate::core::app_state) sessions: Vec<Session>,
    pub(in crate::core::app_state) active_session_id: String,
    pub(in crate::core::app_state) model_config: Option<ModelProviderConfig>,
    pub(in crate::core::app_state) model_list: Vec<String>,
    pub(in crate::core::app_state) agent_config: Option<AgentConfig>,
}

#[derive(Debug)]
pub(in crate::core::app_state) enum TurnEvent {
    PlanReady(TaskPlan),
    LlmOutput(LlmOutputRecord),
    ToolExecution(ToolResult),
    PlanExecutionSummary(String),
    /// 阶段性流式 thinking，直接追加到对应 stage 的系统消息中
    StageThinking {
        stage: String,
        delta: ModelStreamChunk,
    },
    /// 响应阶段的流式输出，追加到 assistant 消息中
    Chunk(ModelStreamChunk),
    Completed(Box<TurnExecution>),
    Failed(String),
}

#[derive(Debug)]
pub(in crate::core::app_state) struct PendingTurn {
    pub(in crate::core::app_state) session_id: String,
    pub(in crate::core::app_state) task_id: String,
    pub(in crate::core::app_state) assistant_message_id: Option<String>,
    /// 当前 stage thinking 对应的系统消息 ID，用于流式追加
    pub(in crate::core::app_state) stage_thinking_message_id: Option<String>,
    pub(in crate::core::app_state) started_at: Instant,
    pub(in crate::core::app_state) rx: Receiver<TurnEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(in crate::core::app_state) struct SkillsLockRecord {
    pub(in crate::core::app_state) version: String,
    pub(in crate::core::app_state) enabled: bool,
    pub(in crate::core::app_state) source: String,
    pub(in crate::core::app_state) installed_at: String,
    pub(in crate::core::app_state) managed_mcp_servers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(in crate::core::app_state) struct McpDependencyLockRecord {
    pub(in crate::core::app_state) path: String,
    pub(in crate::core::app_state) ref_count: usize,
    pub(in crate::core::app_state) installed_at: String,
}

#[derive(Debug)]
pub(in crate::core::app_state) struct ScopedDirCleanup {
    dir: Option<PathBuf>,
}

impl ScopedDirCleanup {
    pub(in crate::core::app_state) fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    pub(in crate::core::app_state) fn is_enabled(&self) -> bool {
        self.dir.is_some()
    }
}

impl Drop for ScopedDirCleanup {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

#[derive(Debug)]
pub(in crate::core::app_state) struct AppPaths {
    pub(in crate::core::app_state) app_storage_path: PathBuf,
    pub(in crate::core::app_state) skills_config_path: PathBuf,
    pub(in crate::core::app_state) mcp_config_path: PathBuf,
    pub(in crate::core::app_state) mcp_capability_cache_path: PathBuf,
    pub(in crate::core::app_state) sessions_dir_path: PathBuf,
}

#[derive(Debug)]
pub(in crate::core::app_state) struct AppServices {
    pub(in crate::core::app_state) skill_service: AppSkillService,
    pub(in crate::core::app_state) mcp_service: AppMcpService,
    pub(in crate::core::app_state) repository: AppRepository,
    pub(in crate::core::app_state) runtime: RuntimeEngine,
    pub(in crate::core::app_state) turn_service: AppTurnService,
}
