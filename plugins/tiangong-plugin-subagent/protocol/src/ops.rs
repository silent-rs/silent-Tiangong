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

pub const TOOL_CREATE_AGENT: &str = "create_agent";
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
pub const TOOL_GET_AGENT_MEMORY: &str = "get_agent_memory";
pub const TOOL_APPEND_AGENT_MEMORY: &str = "append_agent_memory";
pub const TOOL_APPEND_AGENT_INSTRUCTIONS: &str = "append_agent_instructions";
pub const TOOL_REPORT_AGENT_RESULT: &str = "report_agent_result";

/// 全部工具操作名（与 WASM tool-specs 一一对应）。
pub const TOOL_OPERATIONS: &[&str] = &[
    TOOL_CREATE_AGENT,
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
    TOOL_GET_AGENT_MEMORY,
    TOOL_APPEND_AGENT_MEMORY,
    TOOL_APPEND_AGENT_INSTRUCTIONS,
    TOOL_REPORT_AGENT_RESULT,
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
pub const UI_LIST_SESSIONS: &str = "ui_list_sessions";
pub const UI_LIST_MEMORY: &str = "ui_list_memory";
pub const UI_READ_MEMORY: &str = "ui_read_memory";
pub const UI_WRITE_MEMORY: &str = "ui_write_memory";
pub const UI_DELETE_MEMORY: &str = "ui_delete_memory";
pub const UI_COMPILE_MEMORY: &str = "ui_compile_memory";

/// 优雅关闭操作（宿主退出流程在终止 sidecar 前调用）。
pub const SHUTDOWN_OPERATION: &str = "subagent_shutdown";

/// WASM 生命周期钩子转发操作：关联会话本轮完成（on_turn_finished → sidecar）。
pub const SESSION_TURN_FINISHED: &str = "session_turn_finished";

/// @ 提及候选查询（WASM mention-candidates → sidecar）：返回启用 Agent 的候选列表。
pub const MENTION_CANDIDATES: &str = "mention_candidates";

// ── 请求类型 ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AgentIdRequest {
    pub agent_id: String,
}

/// 招募缺省后端：原生 Subagent（无需会话或命令线索）。
fn default_recruit_backend() -> BackendKind {
    BackendKind::AgentTeam
}

/// AI 招募请求：创建（或复用同名）持久 Subagent 并在当前会话激活。
#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    /// 成员名称；同名 Agent 已存在时直接复用（延续其指令与记忆）。
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 运行后端：agent_team（原生，默认，无需额外参数）、cli（需 command）
    /// 或 tiangong_session（需 session_id / session_query 之一）。
    #[serde(default = "default_recruit_backend")]
    pub backend: BackendKind,
    #[serde(default)]
    pub command: Option<String>,
    /// 关联会话 ID（tiangong_session）。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 按标题关键词搜索会话并取最近匹配（tiangong_session 的替代写法）。
    #[serde(default)]
    pub session_query: Option<String>,
    #[serde(default)]
    pub workspace_policy: Option<WorkspacePolicy>,
    #[serde(default)]
    pub instructions: Option<String>,
    /// 创建后是否立即在当前会话激活，默认 true。
    #[serde(default)]
    pub activate: Option<bool>,
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
    /// 天工会话后端：关联的源会话 ID。
    #[serde(default)]
    pub session_id: Option<String>,
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
    pub session_id: Option<String>,
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

#[derive(Debug, Deserialize)]
pub struct SessionTurnFinishedRequest {
    /// 完成本轮的会话（Agent 关联的源会话）。
    pub session_id: String,
    /// 本轮用户消息文本（用于确认该轮由 Subagent 投递触发，防止误归因）。
    #[serde(default)]
    pub user_text: String,
    /// 本轮最终回复文本（assistant 消息 text 块拼接）。
    #[serde(default)]
    pub assistant_text: String,
    /// 本轮用户消息锚点的 turn 终态：success / failed / cancelled。
    #[serde(default)]
    pub turn_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionBrief {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub message_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct UiReadMemoryRequest {
    pub agent_id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct UiWriteMemoryRequest {
    pub agent_id: String,
    pub name: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct UiDeleteMemoryRequest {
    pub agent_id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct AgentMemoryRequest {
    pub agent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AppendAgentMemoryRequest {
    pub agent_id: String,
    pub content: String,
    #[serde(default)]
    pub note: Option<String>,
    /// 目标记忆文件（默认 notes.md；可复用经验写 lessons.md）。
    #[serde(default)]
    pub memory_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AppendAgentInstructionsRequest {
    pub agent_id: String,
    /// 追加的稳定规则（带日期分段写入，不覆盖既有内容）。
    pub addition: String,
}

/// 成员主动回报：向当前工作的发起方投递结果（带状态与任务归属），
/// 并终结对应的运行记录。
#[derive(Debug, Deserialize)]
pub struct ReportAgentResultRequest {
    /// 回报结果正文。
    pub result: String,
    /// 回报状态：completed（默认）/ failed / blocked。
    #[serde(default)]
    pub status: Option<String>,
    /// 可选备注（产物位置、后续建议等）。
    #[serde(default)]
    pub note: Option<String>,
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
    /// 长期指令（instructions.md 全文；管理页查看与编辑用）。
    #[serde(default)]
    pub instructions: String,
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

/// Agent 长期记忆文件条目（memory/ 下 markdown）。
#[derive(Debug, Clone, Serialize)]
pub struct MemoryFileEntry {
    pub name: String,
    pub size_bytes: u64,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryReadOutcome {
    pub name: String,
    pub content: String,
}
