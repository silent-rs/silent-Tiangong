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
    /// 会话级输入草稿。发送状态为运行时字段，不会写入此映射。
    #[serde(default)]
    pub(in crate::app_state) input_drafts: HashMap<String, SessionInputDraft>,
    /// 旧版全局草稿兼容字段。加载后迁移到当时的 active_session_id，后续不再写回。
    #[serde(default, skip_serializing)]
    pub(in crate::app_state) input_draft: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::app_state) struct LegacyPersistedState {
    pub(in crate::app_state) sessions: Vec<Session>,
    pub(in crate::app_state) active_session_id: String,
    // model_config（旧版 ModelProviderConfig）已随 ModelProviderConfig 一并移除；
    // 旧 app.json 中残留的同名字段会被 serde 默认忽略。
    #[serde(default)]
    pub(in crate::app_state) model_list: Vec<String>,
    #[serde(default)]
    pub(in crate::app_state) input_draft: String,
}

#[derive(Debug)]
pub(in crate::app_state) struct LoadedState {
    pub(in crate::app_state) sessions: Vec<Session>,
    pub(in crate::app_state) active_session_id: String,
    pub(in crate::app_state) workspace_dir: String,
    pub(in crate::app_state) model_list: Vec<String>,
    pub(in crate::app_state) agent_config: Option<AgentConfig>,
    pub(in crate::app_state) input_drafts: HashMap<String, SessionInputDraft>,
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
    /// 轻量运行时配置缓存（issue #245:替代原 RuntimeEngine）。
    /// 仅保留 UI 展示与派生量计算所需的 context_limit + provider_label,
    /// 不再持有 client / tool_overrides / plugin provider registry——这些
    /// 由 Core 每 turn 现建,app-state 不执行 turn。
    pub runtime: RuntimeConfig,
    pub turn_service: AppTurnService,
}

/// 运行时配置缓存:从 models_config 派生的两个轻量字段。
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// 当前 chat 模型的上下文窗口上限(供会话派生 context_limit /
    /// compression_threshold)。
    pub context_limit: usize,
}
