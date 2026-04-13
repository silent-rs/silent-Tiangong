use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::agent_config::{
    AgentConfig, InstalledSkillConfig, McpConfig, McpServerConfig, McpTransportMode, SkillsConfig,
};
use crate::agents::skill_convert_agent::convert_external_skill_with_agent;
use crate::mcp::{
    McpToolMeta, cached_server_tools, configure_mcp_capability_scheduler, describe_mcp_servers,
    load_mcp_capabilities_cache, refresh_mcp_capabilities_async, summarize_mcp_servers,
    validate_mcp_config,
};
use crate::model::{ModelProviderConfig, SingleProviderClient};
use crate::planner::{PlanStepStatus, TaskPlan};
use crate::runtime::{
    LlmOutputRecord, RunSnapshot, RunStatus, RuntimeEngine, TurnExecution, VerifyExecutionRecord,
};
use crate::session::{MessageRole, Session, SessionTaskPlan, now_text};
use crate::skill::{
    analyze_external_skill, init_tiangong_skill_scaffold, load_skill_from_local_dir,
    prepare_skill_source_for_install,
};
use crate::tool::{ToolExecutionRecord, ToolResult};

pub(crate) mod audit;
mod facade;
pub(crate) mod formatting;
mod repository;
mod services;
mod store;
mod support;
#[cfg(test)]
mod tests;

// Private imports
use self::repository::{
    cleanup_empty_skill_install_dirs, converted_stage_cleanup_dir, copy_dir_recursive,
    default_app_storage_path, default_mcp_capability_cache_path, default_mcp_config_path,
    default_sessions_dir_path, default_skills_config_path, default_skills_storage_dir_path,
    ensure_dir, normalize_model_list, parse_bool, parse_list_value, validate_agent_config,
};
use self::services::{AppMcpService, AppSkillService, AppTurnService};
pub use self::support::StreamEvent;
use self::support::{
    LegacyPersistedState, LoadedState, McpDependencyLockRecord, PersistedAppState,
    ScopedDirCleanup, SkillsLockRecord,
};

// Public re-exports for Tauri API
pub use self::repository::AppRepository;
pub use self::store::{AgentState, AppStore, ProviderState, RuntimeState, SessionState};
pub use self::support::{AppPaths, AppServices, ManagementCommand};

const DEFAULT_SESSION_TITLE: &str = "默认会话";
/// 默认上下文窗口大小（token 数）
/// 大多数模型支持 8K-128K，使用 32K 作为安全默认值
const DEFAULT_CONTEXT_LIMIT: usize = 32_768;
const MCP_CAPABILITY_REFRESH_INTERVAL_SECS: u64 = 300;

#[derive(Debug, Clone, Default)]
pub struct RegisterMcpServerOptions {
    pub transport: Option<McpTransportMode>,
    pub endpoint: Option<String>,
    pub auth_header: Option<String>,
    pub headers: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RegisterMcpServerRequest {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub options: RegisterMcpServerOptions,
}

#[derive(Debug, Clone, Default)]
pub struct SkillInstallInspection {
    pub dependencies: Vec<String>,
    pub env_vars: Vec<String>,
    pub missing_env_vars: Vec<String>,
}

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

    pub fn set_trust_mode(&mut self, mode: crate::permission::TrustMode) -> Result<()> {
        self.store.agent.agent_config.trust_mode = mode;
        // 实时更新共享权限模式（运行中的任务立即生效，因为共享同一个 Arc<RwLock>）
        self.services.runtime.permission_gate().set_trust_mode(mode);
        // 重建 RuntimeEngine（保留共享引用，新任务也使用同一个共享状态）
        self.rebuild_runtime_from_current_config();
        // 持久化
        self.persist_to_disk()
    }
}
