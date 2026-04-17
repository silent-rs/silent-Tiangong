//! Memory Actor 消息协议定义

use tokio::sync::oneshot;

use crate::types::{Episode, ExpandedMemory, RecallAnchors, RecallHit, TurnResult};

/// 注入级别
#[derive(Debug, Clone)]
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
    LoadDepth2 {
        node_ids: Vec<String>,
        reply: oneshot::Sender<Vec<ExpandedMemory>>,
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
    Shutdown,
}
