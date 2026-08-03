//! 工作区索引管理操作（list / delete / rebuild / prewarm）。
//!
//! 对应原 Tauri 命令层直接调用 IndexManager 的管理面，改造后统一经 sidecar IPC。

use serde::{Deserialize, Serialize};

use crate::{Empty, IndexOperation, WorkspaceIndexInfo};

pub const LIST_WORKSPACE_INDEXES_OPERATION: &str = "index.list_workspace_indexes";
pub const DELETE_WORKSPACE_INDEX_OPERATION: &str = "index.delete_workspace_index";
pub const REBUILD_WORKSPACE_INDEX_OPERATION: &str = "index.rebuild_workspace_index";
pub const PREWARM_WORKSPACE_INDEX_OPERATION: &str = "index.prewarm_workspace_index";

pub struct ListWorkspaceIndexes;
impl IndexOperation for ListWorkspaceIndexes {
    const NAME: &'static str = LIST_WORKSPACE_INDEXES_OPERATION;
    type Request = Empty;
    type Response = Vec<WorkspaceIndexInfo>;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteWorkspaceIndexRequest {
    pub root: String,
    pub workspace_id: String,
}

pub struct DeleteWorkspaceIndex;
impl IndexOperation for DeleteWorkspaceIndex {
    const NAME: &'static str = DELETE_WORKSPACE_INDEX_OPERATION;
    type Request = DeleteWorkspaceIndexRequest;
    type Response = Empty;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RebuildWorkspaceIndexRequest {
    pub root: String,
}

/// 重建响应。
///
/// `queued=true` 表示重建已排队后台执行（异步模式），`count` 为 0；
/// `queued=false` 表示重建已完成（同步模式），`count` 为扫描条目数。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RebuildWorkspaceIndexResponse {
    #[serde(default)]
    pub queued: bool,
    #[serde(default)]
    pub count: usize,
}

pub struct RebuildWorkspaceIndex;
impl IndexOperation for RebuildWorkspaceIndex {
    const NAME: &'static str = REBUILD_WORKSPACE_INDEX_OPERATION;
    type Request = RebuildWorkspaceIndexRequest;
    type Response = RebuildWorkspaceIndexResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrewarmWorkspaceIndexRequest {
    pub root: String,
}

pub struct PrewarmWorkspaceIndex;
impl IndexOperation for PrewarmWorkspaceIndex {
    const NAME: &'static str = PREWARM_WORKSPACE_INDEX_OPERATION;
    type Request = PrewarmWorkspaceIndexRequest;
    type Response = Empty;
}
