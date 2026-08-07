use serde::{Deserialize, Serialize};

use crate::MemoryOperation;

pub const RECALL_OPERATION: &str = "recall";
pub const RECALL_CONTEXT_OPERATION: &str = "recall_context";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Episode,
    Entity,
    Decision,
    Evidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchStrategy {
    Skip,
    Keyword,
    Semantic,
    Hybrid { semantic_ratio: f64 },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallAnchors {
    #[serde(default)]
    pub keywords: Vec<String>,
    pub query: String,
    #[serde(default)]
    pub strategy: Option<SearchStrategy>,
}

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

pub struct Recall;

impl MemoryOperation for Recall {
    const NAME: &'static str = RECALL_OPERATION;
    type Request = RecallRequest;
    type Response = RecallResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallRequest {
    pub anchors: RecallAnchors,
    pub limit: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallResponse {
    #[serde(default)]
    pub hits: Vec<RecallHit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallDepth {
    Skip,
    Simple,
    #[default]
    Normal,
    Deep,
}

pub struct RecallContext;

impl MemoryOperation for RecallContext {
    const NAME: &'static str = RECALL_CONTEXT_OPERATION;
    type Request = RecallContextRequest;
    type Response = RecallContextResult;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallQuery {
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallContextRequest {
    pub request: RecallQuery,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallContextResponse {
    pub content: String,
    #[serde(default)]
    pub hits: Vec<RecallHit>,
    #[serde(default)]
    pub used_llm: bool,
    #[serde(default)]
    pub recall_depth: RecallDepth,
    #[serde(default)]
    pub deep_queries: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallContextResult {
    pub response: RecallContextResponse,
}
