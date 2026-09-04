//! 操作名与请求/响应类型。
//!
//! 两组入口：
//! - AI 工具操作（operation 等于工具名）：会话归属以宿主注入的 invocation
//!   context 为准，忽略请求参数中的会话字段；
//! - UI 操作（`ui_*`）：由管理页显式携带 session_id / workspace。

use serde::{Deserialize, Serialize};

use crate::config::{AdapterCapabilities, AgentConfig, BackendKind, WorkspacePolicy};
use crate::state::{ActivationRecord, AgentEventRecord, RunRecord, RunStatus, TaskRecord};

// ── AI 工具操作名 ──────────────────────────────────────────────

pub const TOOL_LIST_AGENTS: &str = "list_agents";
pub const TOOL_GET_AGENT: &str = "get_agent";
pub const TOOL_ACTIVATE_AGENT: &str = "activate_agent";
pub const TOOL_DEACTIVATE_AGENT: &str = "deactivate_agent";
pub const TOOL_LIST_ACTIVE_AGENTS: &str = "list_active_agents";
pub const TOOL_SEND_AGENT_MESSAGE: &str = "send_agent_message";
pub const TOOL_SUBMIT_AGENT_TASK: &str = "submit_agent_task";
pub const TOOL_GET_AGENT_TASK: &str = "get_agent_task";
pub const TOOL_LIST_AGENT_TASKS: &str = "list_agent_tasks";
pub const TOOL_GET_AGENT_RUN: &str = "get_agent_run";
pub const TOOL_INTERRUPT_AGENT_RUN: &str = "interrupt_agent_run";
pub const TOOL_CANCEL_AGENT_RUN: &str = "cancel_agent_run";
pub const TOOL_LIST_AGENT_EVENTS: &str = "list_agent_events";
pub const TOOL_GET_AGENT_ARTIFACTS: &str = "get_agent_artifacts";

/// 全部工具操作名（与 plugin.json tools[] 一一对应）。
pub const TOOL_OPERATIONS: &[&str] = &[
    TOOL_LIST_AGENTS,
    TOOL_GET_AGENT,
    TOOL_ACTIVATE_AGENT,
    TOOL_DEACTIVATE_AGENT,
    TOOL_LIST_ACTIVE_AGENTS,
    TOOL_SEND_AGENT_MESSAGE,
    TOOL_SUBMIT_AGENT_TASK,
    TOOL_GET_AGENT_TASK,
    TOOL_LIST_AGENT_TASKS,
    TOOL_GET_AGENT_RUN,
    TOOL_INTERRUPT_AGENT_RUN,
    TOOL_CANCEL_AGENT_RUN,
    TOOL_LIST_AGENT_EVENTS,
    TOOL_GET_AGENT_ARTIFACTS,
];

// ── UI 操作名 ──────────────────────────────────────────────────

pub const UI_STATE_SNAPSHOT: &str = "ui_state_snapshot";
pub const UI_AGENT_CREATE: &str = "ui_agent_create";
pub const UI_AGENT_UPDATE: &str = "ui_agent_update";
pub const UI_AGENT_DELETE: &str = "ui_agent_delete";
pub const UI_ACTIVATE: &str = "ui_activate";
pub const UI_DEACTIVATE: &str = "ui_deactivate";
pub const UI_SEND_MESSAGE: &str = "ui_send_message";
pub const UI_SUBMIT_TASK: &str = "ui_submit_task";
pub const UI_INTERRUPT_RUN: &str = "ui_interrupt_run";
pub const UI_CANCEL_RUN: &str = "ui_cancel_run";

/// 优雅关闭操作（宿主退出流程在终止 sidecar 前调用）。
pub const SHUTDOWN_OPERATION: &str = "subagent_shutdown";

// ── 请求类型 ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AgentIdRequest {
    pub agent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub agent_id: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct SubmitTaskRequest {
    pub agent_id: String,
    pub goal: String,
    #[serde(default)]
    pub completion_criteria: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TaskIdRequest {
    pub task_id: String,
}

#[derive(Debug, Deserialize)]
pub struct RunIdRequest {
    pub run_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ListAgentTasksRequest {
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListAgentEventsRequest {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct UiActivateRequest {
    pub agent_id: String,
    pub session_id: String,
    pub workspace: String,
}

#[derive(Debug, Deserialize)]
pub struct UiDeactivateRequest {
    pub agent_id: String,
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct UiSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct UiSendMessageRequest {
    pub agent_id: String,
    pub session_id: String,
    pub workspace: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct UiSubmitTaskRequest {
    pub agent_id: String,
    pub session_id: String,
    pub workspace: String,
    pub goal: String,
    #[serde(default)]
    pub completion_criteria: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UiAgentCreateRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub backend: BackendKind,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub workspace_policy: Option<WorkspacePolicy>,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UiAgentUpdateRequest {
    pub agent_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub workspace_policy: Option<WorkspacePolicy>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UiAgentDeleteRequest {
    pub agent_id: String,
}

// ── 响应类型 ───────────────────────────────────────────────────

/// Agent 概览：身份 + 当前会话激活 + 运行实例状态。
#[derive(Debug, Clone, Serialize)]
pub struct AgentSummary {
    pub config: AgentConfig,
    pub capabilities: AdapterCapabilities,
    /// 该 Agent 当前所有活跃激活。
    pub activations: Vec<ActivationRecord>,
    /// 本会话是否已激活（无会话上下文时为 false）。
    #[serde(default)]
    pub activated_in_session: bool,
    /// 当前运行实例状态（任一激活上的活跃 run；无则空闲）。
    pub runtime_status: Option<RunStatus>,
    /// 活跃 run id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentDetail {
    pub summary: AgentSummary,
    /// 长期指令（instructions.md）。
    pub instructions: String,
    pub recent_tasks: Vec<TaskRecord>,
    pub recent_events: Vec<AgentEventRecord>,
    pub artifact_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendOutcome {
    pub run_id: String,
    /// 消息注入了既有运行还是新建了运行。
    pub injected_into_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmitOutcome {
    pub task_id: String,
    pub run_id: String,
}

/// 管理页一次性快照。
#[derive(Debug, Clone, Serialize)]
pub struct StateSnapshot {
    pub agents: Vec<AgentSummary>,
    pub session_id: String,
    pub active_tasks: Vec<TaskRecord>,
    pub recent_runs: Vec<RunRecord>,
    pub recent_events: Vec<AgentEventRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactEntry {
    pub name: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
}
