use serde::{Deserialize, Serialize};

use crate::recall::{RecallAnchors, RecallRequest, RecallResponse};
use crate::{Ack, Empty, MemoryOperation};

pub const CONFIG_GET_OPERATION: &str = "ui.memory.config.get";
pub const CONFIG_SET_OPERATION: &str = "ui.memory.config.set";
pub const LIST_NODES_OPERATION: &str = "list_nodes";
pub const COUNT_NODES_OPERATION: &str = "count_nodes";
pub const LIST_RELATIONS_OPERATION: &str = "list_relations";
pub const LIST_RELATIONS_BATCH_OPERATION: &str = "list_relations_batch";
pub const UPSERT_MANUAL_MEMORY_OPERATION: &str = "upsert_manual_memory";
pub const SET_NODE_STATUS_OPERATION: &str = "set_node_status";
pub const UPSERT_RELATION_OPERATION: &str = "upsert_relation";
pub const DELETE_RELATION_OPERATION: &str = "delete_relation";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemorySelection {
    pub model_key: Option<String>,
    pub embedding_key: Option<String>,
    pub rerank_key: Option<String>,
    #[serde(default = "default_vector_mode")]
    pub vector_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryUiModel {
    pub key: String,
    pub provider: String,
    pub model: String,
    pub capabilities: Vec<String>,
    pub dimension: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBootstrap {
    pub config: MemorySelection,
    pub models: Vec<MemoryUiModel>,
    #[serde(default)]
    pub disabled: bool,
}

pub struct GetConfig;

impl MemoryOperation for GetConfig {
    const NAME: &'static str = CONFIG_GET_OPERATION;
    type Request = Empty;
    type Response = MemoryBootstrap;
}

pub struct SetConfig;

impl MemoryOperation for SetConfig {
    const NAME: &'static str = CONFIG_SET_OPERATION;
    type Request = MemorySelection;
    type Response = Ack;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Episode,
    Entity,
    Decision,
    Evidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCognitiveType {
    #[default]
    Factual,
    UserPreference,
    UserHabit,
    Skill,
    ProjectStructure,
    ArchitectureDecision,
    ProblemIncident,
    DomainKnowledge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScopeType {
    Global,
    Workspace,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    #[default]
    Active,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRelationKind {
    #[default]
    RelatedTo,
    DependsOn,
    Supports,
    Contradicts,
    Supersedes,
    CausedBy,
    BelongsTo,
    LearnedFrom,
    ValidatedBy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNode {
    pub id: String,
    pub kind: MemoryKind,
    #[serde(default)]
    pub memory_type: MemoryCognitiveType,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManualMemoryDraft {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub memory_type: MemoryCognitiveType,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub importance: f32,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryListQuery {
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub status: Option<MemoryStatus>,
    #[serde(default)]
    pub created_after: Option<String>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRelation {
    pub id: String,
    pub from_node_id: String,
    pub to_node_id: String,
    pub relation_kind: MemoryRelationKind,
    pub weight: f32,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryRelationDraft {
    #[serde(default)]
    pub id: Option<String>,
    pub from_node_id: String,
    pub to_node_id: String,
    #[serde(default)]
    pub relation_kind: MemoryRelationKind,
    #[serde(default)]
    pub weight: f32,
    #[serde(default)]
    pub note: Option<String>,
}

macro_rules! operation {
    ($name:ident, $operation:expr, $request:ty, $response:ty) => {
        pub struct $name;
        impl MemoryOperation for $name {
            const NAME: &'static str = $operation;
            type Request = $request;
            type Response = $response;
        }
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListNodesRequest {
    pub query: MemoryListQuery,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodesResponse {
    #[serde(default)]
    pub items: Vec<MemoryNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountNodesRequest {
    pub query: MemoryListQuery,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeCountResponse {
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRelationsRequest {
    pub node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRelationsBatchRequest {
    pub node_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelationsResponse {
    #[serde(default)]
    pub items: Vec<MemoryRelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertManualMemoryRequest {
    pub draft: ManualMemoryDraft,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResponse {
    pub item: MemoryNode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetNodeStatusRequest {
    pub node_id: String,
    pub status: MemoryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertRelationRequest {
    pub draft: MemoryRelationDraft,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationResponse {
    pub item: MemoryRelation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteRelationRequest {
    pub relation_id: String,
}

operation!(
    ListNodes,
    LIST_NODES_OPERATION,
    ListNodesRequest,
    NodesResponse
);
operation!(
    CountNodes,
    COUNT_NODES_OPERATION,
    CountNodesRequest,
    NodeCountResponse
);
operation!(
    ListRelations,
    LIST_RELATIONS_OPERATION,
    ListRelationsRequest,
    RelationsResponse
);
operation!(
    ListRelationsBatch,
    LIST_RELATIONS_BATCH_OPERATION,
    ListRelationsBatchRequest,
    RelationsResponse
);
operation!(
    UpsertManualMemory,
    UPSERT_MANUAL_MEMORY_OPERATION,
    UpsertManualMemoryRequest,
    NodeResponse
);
operation!(
    SetNodeStatus,
    SET_NODE_STATUS_OPERATION,
    SetNodeStatusRequest,
    Ack
);
operation!(
    UpsertRelation,
    UPSERT_RELATION_OPERATION,
    UpsertRelationRequest,
    RelationResponse
);
operation!(
    DeleteRelation,
    DELETE_RELATION_OPERATION,
    DeleteRelationRequest,
    Ack
);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum UiRequest {
    ListNodes {
        query: MemoryListQuery,
    },
    CountNodes {
        query: MemoryListQuery,
    },
    ListRelations {
        node_id: String,
    },
    ListRelationsBatch {
        node_ids: Vec<String>,
    },
    UpsertManualMemory {
        draft: ManualMemoryDraft,
    },
    SetNodeStatus {
        node_id: String,
        status: MemoryStatus,
    },
    UpsertRelation {
        draft: MemoryRelationDraft,
    },
    DeleteRelation {
        relation_id: String,
    },
    Recall {
        anchors: RecallAnchors,
        limit: usize,
    },
}

impl From<RecallRequest> for UiRequest {
    fn from(request: RecallRequest) -> Self {
        Self::Recall {
            anchors: request.anchors,
            limit: request.limit,
        }
    }
}

pub type UiRecallResponse = RecallResponse;

fn default_vector_mode() -> String {
    "auto".to_string()
}
