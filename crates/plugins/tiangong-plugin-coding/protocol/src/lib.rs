//! Coding 插件私有业务协议。
//!
//! 本 crate 只定义 WASM 与 sidecar 共享的操作和数据结构，不包含宿主实现细节。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PLUGIN_ID: &str = "coding";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CODING_PROTOCOL_VERSION: u32 = 2;

pub const TOOL_PROJECT_CONTEXT: &str = "coding_project_context";
pub const TOOL_PREFLIGHT: &str = "coding_preflight";
pub const TOOL_CHECKPOINT: &str = "coding_checkpoint";
pub const TOOL_REVIEW: &str = "coding_review";

pub trait CodingOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

pub struct ProjectContext;
pub struct Preflight;
pub struct Checkpoint;
pub struct Review;

impl CodingOperation for ProjectContext {
    const NAME: &'static str = "project_context";
    type Request = WorkspaceRequest;
    type Response = ProjectContextResponse;
}

impl CodingOperation for Preflight {
    const NAME: &'static str = "preflight";
    type Request = PreflightRequest;
    type Response = PreflightResponse;
}

impl CodingOperation for Checkpoint {
    const NAME: &'static str = "checkpoint";
    type Request = CheckpointRequest;
    type Response = CheckpointResponse;
}

impl CodingOperation for Review {
    const NAME: &'static str = "review";
    type Request = ReviewRequest;
    type Response = ReviewResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRequest {
    pub workspace: String,
    pub full_trust: bool,
    #[serde(default)]
    pub task: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecommendedCheck {
    pub cwd: String,
    pub command: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectContextResponse {
    pub workspace: String,
    pub full_trust: bool,
    pub project_types: Vec<String>,
    pub project_files: Vec<String>,
    pub rule_files: Vec<String>,
    pub workflow_files: Vec<String>,
    pub version_controlled: bool,
    pub version_control_inspected: bool,
    pub git_branch: Option<String>,
    pub has_uncommitted_changes: bool,
    pub recommended_checks: Vec<RecommendedCheck>,
    pub latest_checkpoint: Option<SavedCheckpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightRequest {
    pub workspace: String,
    pub full_trust: bool,
    pub task: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreflightResponse {
    pub workspace: String,
    pub task: String,
    pub version_controlled: bool,
    pub version_control_inspected: bool,
    pub git_branch: Option<String>,
    pub has_uncommitted_changes: bool,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub completion_criteria: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationResult {
    pub name: String,
    pub passed: bool,
    #[serde(default)]
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub workspace: String,
    pub task: String,
    pub completion_criteria: Vec<String>,
    pub completed: Vec<String>,
    pub changed_files: Vec<String>,
    pub verification: Vec<VerificationResult>,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCheckpoint {
    pub saved_at: String,
    pub state: CheckpointRequest,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckpointResponse {
    pub saved_at: String,
    pub checkpoint_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub workspace: String,
    pub base_ref: Option<String>,
    pub allowed_paths: Vec<String>,
    pub verification: Vec<VerificationResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewResponse {
    pub version_controlled: bool,
    pub version_control_inspected: bool,
    pub base_ref: Option<String>,
    pub merge_base: Option<String>,
    pub changed_files: Vec<String>,
    pub unexpected_files: Vec<String>,
    pub has_uncommitted_changes: bool,
    pub has_committed_changes: bool,
    pub verification_complete: bool,
    pub failed_verifications: Vec<String>,
    pub ready: bool,
    pub notes: Vec<String>,
}
