//! 生命周期钩子链路操作（set_workspace / index_turn_batch / finalize_session）。

use serde::{Deserialize, Serialize};

use crate::{Ack, Empty, IndexOperation, TurnData};

pub const SET_WORKSPACE_OPERATION: &str = "index.set_workspace";
pub const INDEX_TURN_BATCH_OPERATION: &str = "index.index_turn_batch";
pub const FINALIZE_SESSION_OPERATION: &str = "index.finalize_session";

/// `set_workspace` 钩子请求：通知 sidecar 工作区变更并触发后台扫描。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetWorkspaceRequest {
    /// 新工作目录；None 表示清除。
    #[serde(default)]
    pub workspace: Option<String>,
}

pub struct SetWorkspace;
impl IndexOperation for SetWorkspace {
    const NAME: &'static str = SET_WORKSPACE_OPERATION;
    type Request = SetWorkspaceRequest;
    type Response = Ack;
}

/// `on_turn_finished` 钩子请求：批量写入本轮对话索引。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexTurnBatchRequest {
    pub session_id: String,
    pub turns: Vec<TurnData>,
}

pub struct IndexTurnBatch;
impl IndexOperation for IndexTurnBatch {
    const NAME: &'static str = INDEX_TURN_BATCH_OPERATION;
    type Request = IndexTurnBatchRequest;
    type Response = Ack;
}

/// `on_session_ended` 钩子请求：finalize 会话索引。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FinalizeSessionRequest {
    pub session_id: String,
}

pub struct FinalizeSession;
impl IndexOperation for FinalizeSession {
    const NAME: &'static str = FINALIZE_SESSION_OPERATION;
    type Request = FinalizeSessionRequest;
    type Response = Empty;
}
