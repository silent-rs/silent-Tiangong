use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::core::agent_config::{
    AgentConfig, InstalledSkillConfig, McpConfig, McpServerConfig, McpTransportMode, SkillsConfig,
};
use crate::core::agents::skill_convert_agent::convert_external_skill_with_agent;
use crate::core::mcp::{
    McpToolMeta, cached_server_tools, configure_mcp_capability_scheduler, describe_mcp_servers,
    load_mcp_capabilities_cache, refresh_mcp_capabilities_async, summarize_mcp_servers,
    validate_mcp_config,
};
use crate::core::model::{ModelProviderConfig, ModelStreamChunk, SingleProviderClient};
use crate::core::planner::{PlanStepStatus, TaskPlan};
use crate::core::runtime::{
    LlmOutputRecord, RunSnapshot, RunStatus, RuntimeEngine, TurnExecution, VerifyExecutionRecord,
};
use crate::core::session::{Message, MessageRole, Session, SessionTaskPlan, now_text};
use crate::core::skill::{
    analyze_external_skill, init_tiangong_skill_scaffold, load_skill_from_local_dir,
    prepare_skill_source_for_install,
};
use crate::core::tool::{ToolExecutionRecord, ToolResult};

mod facade;
mod formatting;
mod repository;
mod services;
mod store;
mod support;
#[cfg(test)]
mod tests;

use self::repository::{
    AppRepository, cleanup_empty_skill_install_dirs, converted_stage_cleanup_dir,
    copy_dir_recursive, default_app_storage_path, default_mcp_capability_cache_path,
    default_mcp_config_path, default_sessions_dir_path, default_skills_config_path,
    default_skills_storage_dir_path, elapsed_ms_u64, ensure_dir, normalize_model_list, parse_bool,
    parse_list_value, user_home_dir, validate_agent_config,
};
use self::services::{AppMcpService, AppSkillService, AppTurnService};
use self::store::{AgentState, AppStore, ProviderState, RuntimeState, SessionState};
use self::support::{
    AppPaths, AppServices, LegacyPersistedState, LoadedState, McpDependencyLockRecord, PendingTurn,
    PersistedAppState, ScopedDirCleanup, SkillsLockRecord, TurnEvent,
};

const DEFAULT_SESSION_TITLE: &str = "默认会话";
const DEFAULT_CONTEXT_LIMIT: usize = 16;
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
    store: AppStore,
    services: AppServices,
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
}
