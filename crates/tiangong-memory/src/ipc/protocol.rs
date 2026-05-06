//! IPC 帧协议定义（TCP loopback + JSON Lines）

use serde::{Deserialize, Serialize};

use crate::command::InjectionLevel;
use crate::types::{
    Episode, ExpandedMemory, ManualMemoryDraft, MemoryListQuery, MemoryNode, MemoryRecallRequest,
    MemoryRecallResponse, MemoryRelation, MemoryRelationDraft, MemoryStatus, RecallAnchors,
    RecallHit, TurnResult,
};

/// Endpoint 发现信息，写入本地 runtime 文件供 follower 读取。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcEndpoint {
    pub service: String,
    pub host: String,
    pub port: u16,
    pub pid: u32,
    pub token: String,
    pub updated_at: String,
}

/// IPC 请求帧
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub request_id: String,
    pub payload: serde_json::Value,
}

/// IPC 响应帧
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub request_id: String,
    pub payload: serde_json::Value,
}

/// 连接建立后的第一帧，使用 token 做本地鉴权。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcAuth {
    pub token: String,
}

/// JSON Lines 传输帧
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcFrame {
    Auth(IpcAuth),
    Request(IpcRequest),
    Response(IpcResponse),
    Error { message: String },
}

/// Memory IPC 请求载荷
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum MemoryIpcRequestPayload {
    LoadInjection {
        session_id: String,
        workspace_id: Option<String>,
    },
    Recall {
        anchors: RecallAnchors,
        limit: usize,
    },
    RecallContext {
        request: MemoryRecallRequest,
    },
    LoadDepth2 {
        node_ids: Vec<String>,
    },
    ListNodes {
        query: MemoryListQuery,
    },
    ListRelations {
        node_id: String,
    },
    ListRelationsBatch {
        node_ids: Vec<String>,
    },
    WriteEpisode {
        episode: Episode,
        workspace_id: Option<String>,
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
    UpdateInjection {
        level: InjectionLevel,
        target_id: String,
        content: String,
    },
    RunMicroRumination {
        turn_result: TurnResult,
    },
    RunMesoRumination {
        session_id: String,
        workspace_id: String,
    },
    RunMetaRumination,
    Shutdown,
}

/// Memory IPC 响应载荷
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryIpcResponsePayload {
    Ack,
    Injection { items: Vec<String> },
    Recall { hits: Vec<RecallHit> },
    RecallContext { response: MemoryRecallResponse },
    Depth2 { items: Vec<ExpandedMemory> },
    Nodes { items: Vec<MemoryNode> },
    Node { item: MemoryNode },
    Relations { items: Vec<MemoryRelation> },
    Relation { item: MemoryRelation },
}
