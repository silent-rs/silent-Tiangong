//! Memory Actor 消息协议定义

use tokio::sync::oneshot;

use crate::options::MemoryOptions;
use crate::types::{
    Episode, ExpandedMemory, ManualMemoryDraft, MemoryListQuery, MemoryNode, MemoryRecallRequest,
    MemoryRecallResponse, MemoryRelation, MemoryRelationDraft, MemoryStatus, RecallAnchors,
    RecallHit, TurnResult,
};

/// 注入级别
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum InjectionLevel {
    Profile,
    Workspace,
    Session,
}

/// Memory Actor 接收的命令
pub enum MemoryCommand {
    // ── 查询类（需要响应）──
    LoadInjection {
        session_id: String,
        workspace_id: Option<String>,
        reply: oneshot::Sender<Vec<String>>,
    },
    Recall {
        anchors: RecallAnchors,
        limit: usize,
        reply: oneshot::Sender<Vec<RecallHit>>,
    },
    RecallContext {
        request: MemoryRecallRequest,
        reply: oneshot::Sender<MemoryRecallResponse>,
    },
    LoadDepth2 {
        node_ids: Vec<String>,
        reply: oneshot::Sender<Vec<ExpandedMemory>>,
    },
    ListNodes {
        query: MemoryListQuery,
        reply: oneshot::Sender<Vec<MemoryNode>>,
    },
    ListRelations {
        node_id: String,
        reply: oneshot::Sender<Vec<MemoryRelation>>,
    },
    /// 批量查询多个节点的关联关系（修复 N+1 性能问题）
    ListRelationsBatch {
        node_ids: Vec<String>,
        reply: oneshot::Sender<Vec<MemoryRelation>>,
    },

    // ── 写入类（fire-and-forget）──
    WriteEpisode {
        episode: Episode,
        /// 显式 workspace_id，为 None 时由 Actor 自身 workspace_id 兜底
        workspace_id: Option<String>,
    },
    UpdateInjection {
        level: InjectionLevel,
        target_id: String,
        content: String,
    },
    UpsertManualMemory {
        draft: ManualMemoryDraft,
        reply: oneshot::Sender<Result<MemoryNode, String>>,
    },
    SetNodeStatus {
        node_id: String,
        status: MemoryStatus,
        reply: oneshot::Sender<Result<(), String>>,
    },
    UpsertRelation {
        draft: MemoryRelationDraft,
        reply: oneshot::Sender<Result<MemoryRelation, String>>,
    },
    DeleteRelation {
        relation_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },

    // ── 反刍类（fire-and-forget）──
    RunMicroRumination {
        turn_result: Box<TurnResult>,
    },
    RunMesoRumination {
        session_id: String,
        workspace_id: String,
    },
    RunMetaRumination,

    // ── 生命周期 ──
    Reconfigure {
        options: Box<MemoryOptions>,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
    Shutdown,
}
