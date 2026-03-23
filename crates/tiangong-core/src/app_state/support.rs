use super::*;

#[derive(Debug, Serialize, Deserialize, Default)]
pub(in crate::app_state) struct PersistedAppState {
    #[serde(default)]
    pub(in crate::app_state) active_session_id: String,
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
    pub(in crate::app_state) model_list: Vec<String>,
    pub(in crate::app_state) agent_config: Option<AgentConfig>,
}

#[derive(Debug)]
pub(in crate::app_state) enum TurnEvent {
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
pub struct PendingTurn {
    pub(in crate::app_state) session_id: String,
    pub(in crate::app_state) task_id: String,
    pub(in crate::app_state) assistant_message_id: Option<String>,
    /// 当前 stage thinking 对应的系统消息 ID，用于流式追加
    pub(in crate::app_state) stage_thinking_message_id: Option<String>,
    pub(in crate::app_state) started_at: Instant,
    pub(in crate::app_state) rx: Receiver<TurnEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(in crate::app_state) struct SkillsLockRecord {
    pub(in crate::app_state) version: String,
    pub(in crate::app_state) enabled: bool,
    pub(in crate::app_state) source: String,
    pub(in crate::app_state) installed_at: String,
    pub(in crate::app_state) managed_mcp_servers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(in crate::app_state) struct McpDependencyLockRecord {
    pub(in crate::app_state) path: String,
    pub(in crate::app_state) ref_count: usize,
    pub(in crate::app_state) installed_at: String,
}

#[derive(Debug)]
pub(in crate::app_state) struct ScopedDirCleanup {
    dir: Option<PathBuf>,
}

impl ScopedDirCleanup {
    pub(in crate::app_state) fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    pub(in crate::app_state) fn is_enabled(&self) -> bool {
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
pub struct AppPaths {
    pub app_storage_path: PathBuf,
    pub skills_config_path: PathBuf,
    pub mcp_config_path: PathBuf,
    pub mcp_capability_cache_path: PathBuf,
    pub sessions_dir_path: PathBuf,
}

#[derive(Debug)]
pub struct AppServices {
    pub skill_service: AppSkillService,
    pub mcp_service: AppMcpService,
    pub repository: AppRepository,
    pub runtime: RuntimeEngine,
    pub turn_service: AppTurnService,
}

#[derive(Debug)]
pub(in crate::app_state) struct InstallRollbackGuard {
    dir: Option<PathBuf>,
}

impl InstallRollbackGuard {
    pub(in crate::app_state) fn new(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }

    pub(in crate::app_state) fn commit(mut self) {
        self.dir = None;
    }
}

impl Drop for InstallRollbackGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir_all(&dir);
            // 清理空父目录
            let mut current = dir.parent().map(std::path::Path::to_path_buf);
            while let Some(p) = current {
                if std::fs::read_dir(&p)
                    .ok()
                    .and_then(|mut d| d.next())
                    .is_some()
                {
                    break;
                }
                let _ = std::fs::remove_dir(&p);
                current = p.parent().map(std::path::Path::to_path_buf);
            }
        }
    }
}
