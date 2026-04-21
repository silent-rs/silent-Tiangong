//! 天工 Memory 系统 — 基础类型定义
//!
//! 定义 Memory 系统所有层次共用的数据结构。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 记忆节点类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Episode,
    Entity,
    Decision,
    Evidence,
}

/// 记忆范围类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScopeType {
    Global,
    Workspace,
    Session,
}

/// 记忆状态
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    #[default]
    Active,
    Archived,
}

/// 记忆节点元数据（对应 memory_nodes 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNode {
    pub id: String,
    pub kind: MemoryKind,
    pub scope_type: MemoryScopeType,
    pub scope_id: Option<String>,
    pub title: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub importance: f32,
    pub confidence: f32,
    pub status: MemoryStatus,
    pub source: Option<String>,
    pub usage_count: i64,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Episode 结果状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeOutcome {
    Success,
    PartialSuccess,
    Failed,
    Abandoned,
}

/// 事件记忆（Episode）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub summary: String,
    pub outcome: EpisodeOutcome,
    pub keywords: Vec<String>,
    pub tool_calls: Vec<String>,
    pub importance: f32,
    pub created_at: String,
}

impl Episode {
    /// 创建新 Episode
    pub fn new(
        session_id: String,
        title: String,
        summary: String,
        outcome: EpisodeOutcome,
        keywords: Vec<String>,
        tool_calls: Vec<String>,
        importance: f32,
    ) -> Self {
        Self {
            id: scru128::new().to_string(),
            session_id,
            title,
            summary,
            outcome,
            keywords,
            tool_calls,
            importance,
            created_at: chrono::Local::now().naive_local().to_string(),
        }
    }
}

/// 实体类型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Project,
    Repository,
    Server,
    Skill,
    Provider,
    Document,
    Module,
}

/// 实体记忆（Entity）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub entity_type: EntityType,
    pub description: String,
    pub file_path: Option<String>,
    pub related_episodes: Vec<String>,
    pub importance: f32,
    pub created_at: String,
    pub updated_at: String,
}

/// 决策记忆（Decision）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub title: String,
    pub context: String,
    pub alternatives: Vec<String>,
    pub chosen: String,
    pub reasons: Vec<String>,
    pub episode_ids: Vec<String>,
    pub created_at: String,
}

/// Profile 记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

/// 检索策略（由 Core/LLM 决定，传入 Memory 执行层）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchStrategy {
    /// 跳过记忆检索（查询不需要历史记忆）
    Skip,
    /// 纯关键词检索（BM25）
    Keyword,
    /// 纯语义检索（向量）
    Semantic,
    /// 混合检索：semantic_ratio 为语义权重占比（0.0~1.0）
    Hybrid { semantic_ratio: f64 },
}

impl Default for SearchStrategy {
    fn default() -> Self {
        SearchStrategy::Hybrid {
            semantic_ratio: 0.5,
        }
    }
}

/// 召回锚点（Phase C 实现）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallAnchors {
    pub keywords: Vec<String>,
    pub query: String,
    /// 检索策略（由 Core 传入，为 None 时 Memory 内部自行判断）
    #[serde(default)]
    pub strategy: Option<SearchStrategy>,
}

/// 召回命中项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallHit {
    pub node_id: String,
    pub title: String,
    pub summary: String,
    pub score: f64,
    pub kind: MemoryKind,
    pub importance: f64,
    pub depth1_loaded: bool,
}

/// Tool 化回忆请求。
///
/// Core 只提供当前请求和最近语境；检索规划、去重和结果整理由 Memory 内部完成。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryRecallRequest {
    pub query: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub expected: Vec<String>,
    #[serde(default)]
    pub context: Vec<String>,
    #[serde(default)]
    pub limit: usize,
}

/// Tool 化回忆响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryRecallResponse {
    pub content: String,
    #[serde(default)]
    pub hits: Vec<RecallHit>,
    #[serde(default)]
    pub used_llm: bool,
}

/// 向量索引点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorPoint {
    pub node_id: String,
    pub title: String,
    pub summary: String,
    pub kind: MemoryKind,
    pub importance: f64,
    pub vector: Vec<f32>,
}

/// 展开的记忆节点（Phase C 实现）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandedMemory {
    pub node_id: String,
    pub full_content: String,
}

/// Turn 中可被长期记忆保存的结构化产物类型。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnArtifactKind {
    Media,
    File,
    #[default]
    ToolResult,
}

/// Turn 中可被长期记忆保存的结构化产物。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnArtifact {
    pub kind: TurnArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Turn 执行结果（Phase B 实现）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnResult {
    pub session_id: String,
    pub turn_id: String,
    pub had_tool_calls: bool,
    #[serde(default)]
    pub user_input: String,
    pub summary: String,
    #[serde(default)]
    pub tool_calls: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<TurnArtifact>,
    /// 当前工作区 ID（显式携带，避免 Actor 固化到启动时工作区）
    pub workspace_id: Option<String>,
}

/// 根据工作区路径生成 workspace_id（SHA-256 前 16 字符）
pub fn workspace_id_from_path(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    hex::encode(&digest[..8]) // 16 字符
}
