//! Fs 工具链路操作（7 工具 + set_workspace）。
//!
//! 所有读写请求都内嵌 [`FsAccessContext`]（沙箱预留点 B），sidecar 据此
//! 做路径解析与信任模式判定。`current_time` 不经 sidecar（wasm 内部用
//! clock host import 实现），故本模块不为其定义操作。

use serde::{Deserialize, Serialize};

use crate::{Ack, FsAccessContext, FsOperation};

// ── 操作名常量 ────────────────────────────────────────────────

pub const LIST_DIR_OPERATION: &str = "fs.list_dir";
pub const TREE_DIR_OPERATION: &str = "fs.tree_dir";
pub const READ_FILE_OPERATION: &str = "fs.read_file";
pub const WRITE_FILE_OPERATION: &str = "fs.write_file";
pub const REPLACE_IN_FILE_OPERATION: &str = "fs.replace_in_file";
pub const APPLY_PATCH_OPERATION: &str = "fs.apply_patch";
pub const SET_WORKSPACE_OPERATION: &str = "fs.set_workspace";

// ── 通用响应（对齐 core ToolResult 字段，便于 sidecar 直接构造）─────

/// Fs 工具响应：保留与 core `ToolResult` 同构字段，便于 sidecar 直接构造。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FsToolResponse {
    pub ok: bool,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// 写工具返回：本次操作实际加锁的路径（供 wasm 发 locked 事件）。
    #[serde(default)]
    pub locked_paths: Vec<String>,
    /// 写工具返回：本次操作实际解锁的路径（供 wasm 发 unlocked 事件）。
    #[serde(default)]
    pub unlocked_paths: Vec<String>,
}

// ── 读工具请求/响应 ───────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListDirRequest {
    /// 目录路径，默认当前目录。
    #[serde(default)]
    pub path: Option<String>,
    #[serde(flatten)]
    pub access: FsAccessContext,
}
pub struct ListDir;
impl FsOperation for ListDir {
    const NAME: &'static str = LIST_DIR_OPERATION;
    type Request = ListDirRequest;
    type Response = FsToolResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeDirRequest {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub max_depth: usize,
    #[serde(flatten)]
    pub access: FsAccessContext,
}
pub struct TreeDir;
impl FsOperation for TreeDir {
    const NAME: &'static str = TREE_DIR_OPERATION;
    type Request = TreeDirRequest;
    type Response = FsToolResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadFileRequest {
    pub path: String,
    #[serde(default)]
    pub start_line: usize,
    #[serde(default)]
    pub max_lines: usize,
    #[serde(flatten)]
    pub access: FsAccessContext,
}
pub struct ReadFile;
impl FsOperation for ReadFile {
    const NAME: &'static str = READ_FILE_OPERATION;
    type Request = ReadFileRequest;
    type Response = FsToolResponse;
}

// ── 写工具请求/响应 ───────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WriteFileRequest {
    pub path: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub append: bool,
    #[serde(flatten)]
    pub access: FsAccessContext,
}
pub struct WriteFile;
impl FsOperation for WriteFile {
    const NAME: &'static str = WRITE_FILE_OPERATION;
    type Request = WriteFileRequest;
    type Response = FsToolResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplaceInFileRequest {
    pub path: String,
    pub old: String,
    #[serde(default)]
    pub new: String,
    #[serde(default)]
    pub replace_all: bool,
    #[serde(default)]
    pub expected_count: Option<usize>,
    #[serde(flatten)]
    pub access: FsAccessContext,
}
pub struct ReplaceInFile;
impl FsOperation for ReplaceInFile {
    const NAME: &'static str = REPLACE_IN_FILE_OPERATION;
    type Request = ReplaceInFileRequest;
    type Response = FsToolResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyPatchRequest {
    pub patch: String,
    #[serde(default)]
    pub verify: bool,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(flatten)]
    pub access: FsAccessContext,
}
pub struct ApplyPatch;
impl FsOperation for ApplyPatch {
    const NAME: &'static str = APPLY_PATCH_OPERATION;
    type Request = ApplyPatchRequest;
    type Response = FsToolResponse;
}

// ── 生命周期 ─────────────────────────────────────────────────

/// `set_workspace` 钩子请求：通知 sidecar 工作区变更（写盘/路径解析基准）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetWorkspaceRequest {
    /// 新工作目录；None 表示清除。
    #[serde(default)]
    pub workspace: Option<String>,
    /// 是否完全信任模式。
    #[serde(default)]
    pub full_trust: bool,
}
pub struct SetWorkspace;
impl FsOperation for SetWorkspace {
    const NAME: &'static str = SET_WORKSPACE_OPERATION;
    type Request = SetWorkspaceRequest;
    type Response = Ack;
}
