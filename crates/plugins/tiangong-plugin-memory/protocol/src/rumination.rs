use serde::{Deserialize, Serialize};

use crate::{Ack, MemoryOperation};

pub const RUN_ENHANCED_MICRO_OPERATION: &str = "run_enhanced_micro_rumination";
pub const RUN_MESO_OPERATION: &str = "run_meso_rumination";

/// 轮次执行状态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    /// 轮次正常完成。
    #[default]
    Completed,
    /// 轮次被用户取消。
    Cancelled,
    /// 轮次因错误结束。
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnArtifactKind {
    Media,
    File,
    #[default]
    ToolResult,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnArtifact {
    pub kind: TurnArtifactKind,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// 标记是否为召回的已有记忆上下文（非新产物）。
    #[serde(default)]
    pub is_recalled_context: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCandidateKind {
    Episode,
    Entity,
    Decision,
    Evidence,
    UserPreference,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCandidate {
    pub tool_name: String,
    pub step_index: usize,
    pub hint: String,
    #[serde(default)]
    pub suggested_kinds: Vec<MemoryCandidateKind>,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub result_summary: Option<String>,
    pub success: bool,
    /// 工具来源分类（如 "memory_recall"、"file_operation"），供 Memory 分析层判断。
    #[serde(default)]
    pub tool_source: Option<String>,
    /// 标记是否为召回的已有记忆（不应作为新记忆写入）。
    #[serde(default)]
    pub is_recalled_context: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnhancedTurnResult {
    pub session_id: String,
    pub turn_id: String,
    pub had_tool_calls: bool,
    /// 轮次执行状态。
    #[serde(default)]
    pub turn_status: TurnStatus,
    #[serde(default)]
    pub user_input: String,
    pub summary: String,
    #[serde(default)]
    pub tool_calls: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<TurnArtifact>,
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub memory_candidates: Vec<MemoryCandidate>,
    #[serde(default)]
    pub turn_messages: Vec<TurnMessage>,
}

pub struct RunEnhancedMicroRumination;

impl MemoryOperation for RunEnhancedMicroRumination {
    const NAME: &'static str = RUN_ENHANCED_MICRO_OPERATION;
    type Request = RunEnhancedMicroRuminationRequest;
    type Response = Ack;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEnhancedMicroRuminationRequest {
    pub turn_result: EnhancedTurnResult,
}

pub struct RunMesoRumination;

impl MemoryOperation for RunMesoRumination {
    const NAME: &'static str = RUN_MESO_OPERATION;
    type Request = RunMesoRuminationRequest;
    type Response = Ack;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMesoRuminationRequest {
    pub session_id: String,
    pub workspace_id: String,
}

pub const RUN_META_OPERATION: &str = "run_meta_rumination";

pub struct RunMetaRumination;

impl MemoryOperation for RunMetaRumination {
    const NAME: &'static str = RUN_META_OPERATION;
    type Request = RunMetaRuminationRequest;
    type Response = Ack;
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunMetaRuminationRequest {}
