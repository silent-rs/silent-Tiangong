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
pub struct Branches;
pub struct Switch;

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

impl CodingOperation for Branches {
    const NAME: &'static str = "branches";
    type Request = BranchesRequest;
    type Response = BranchesResponse;
}

impl CodingOperation for Switch {
    const NAME: &'static str = "switch";
    type Request = SwitchRequest;
    type Response = SwitchResponse;
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
    /// 推荐来源：`rule_file`（项目规则文件提取）、`makefile`、`justfile`、`manifest`（清单推断）。
    #[serde(default = "default_check_source")]
    pub source: String,
}

fn default_check_source() -> String {
    "manifest".to_string()
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
    /// true 表示返回的进度记录与请求任务不匹配（按最近一条回退），仅供参考。
    #[serde(default)]
    pub latest_checkpoint_task_mismatch: bool,
    /// 进度文件损坏等恢复异常的提示；None 表示正常。
    #[serde(default)]
    pub checkpoint_recovery_notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightRequest {
    pub workspace: String,
    pub full_trust: bool,
    pub task: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleFileExcerpt {
    pub path: String,
    /// 规则文件开头的摘要行（已截断），供模型判断是否需要完整读取。
    pub excerpt: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreflightResponse {
    pub workspace: String,
    pub task: String,
    pub version_controlled: bool,
    pub version_control_inspected: bool,
    pub git_branch: Option<String>,
    pub has_uncommitted_changes: bool,
    /// 未提交改动的具体文件（最多 50 条；超出时看 uncommitted_files_total）。
    #[serde(default)]
    pub uncommitted_files: Vec<String>,
    #[serde(default)]
    pub uncommitted_files_total: usize,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub completion_criteria: Vec<String>,
    /// 规则文件内容摘要（每文件截断，总量有上限）。
    #[serde(default)]
    pub rule_file_excerpts: Vec<RuleFileExcerpt>,
    /// true 表示完成标准恢复自该任务此前记录的进度。
    #[serde(default)]
    pub restored_checkpoint: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationEvidence {
    /// 实际执行的命令行。
    pub command: String,
    /// 命令退出码；与 passed 一致才算有效凭据（通过应为 0）。
    pub exit_code: i64,
    /// 输出尾部摘录（可选）。
    #[serde(default)]
    pub output_tail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationResult {
    pub name: String,
    pub passed: bool,
    #[serde(default)]
    pub details: String,
    /// 真实执行痕迹；passed 为 true 时缺失或退出码矛盾则不计入有效验证。
    #[serde(default)]
    pub evidence: Option<VerificationEvidence>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiffStats {
    pub files_changed: usize,
    pub insertions: u64,
    pub deletions: u64,
    pub binary_files: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RiskyFile {
    pub path: String,
    /// 风险类别：`lockfile` / `ci-config` / `deploy-config` / `secret-like`。
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub workspace: String,
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
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
    /// 声明通过但缺少有效执行痕迹（无 evidence 或退出码矛盾）的验证名。
    #[serde(default)]
    pub unverified_claims: Vec<String>,
    /// 空白错误（git diff --check，最多 20 条）。
    #[serde(default)]
    pub whitespace_errors: Vec<String>,
    /// 已提交与工作区改动的合计规模。
    #[serde(default)]
    pub diff_stats: DiffStats,
    /// 需要人工留意的风险文件（锁文件、CI/部署配置、疑似密钥）。
    #[serde(default)]
    pub risky_files: Vec<RiskyFile>,
    pub ready: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchesRequest {
    pub workspace: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BranchInfo {
    /// 本地分支名或 `remote/name` 形式的远端分支名。
    pub name: String,
    pub is_current: bool,
    pub is_remote: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BranchesResponse {
    pub is_repo: bool,
    /// git 探测不可用（命令无法执行/超预算）的说明；与 is_repo=false
    /// （确认非 git 仓库）区分，界面据此显示错误占位而非隐藏。
    #[serde(default)]
    pub check_error: Option<String>,
    /// 当前分支名；分离头指针时为 None。
    pub current: Option<String>,
    pub detached: bool,
    pub head_short: Option<String>,
    pub has_uncommitted_changes: bool,
    pub branches: Vec<BranchInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchRequest {
    pub workspace: String,
    /// 目标分支：本地分支名或 `remote/name` 远端分支（远端无本地对应时自动建跟踪分支）。
    pub branch: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwitchResponse {
    pub switched_to: String,
    /// true 表示从远端分支新建了本地跟踪分支。
    pub created_tracking: bool,
}
