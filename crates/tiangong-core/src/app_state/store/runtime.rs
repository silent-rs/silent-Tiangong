use super::super::*;

#[derive(Debug)]
pub struct RuntimeState {
    pub run: RunSnapshot,
    /// 已废弃：pending_turns 已迁移到 TiangongCore 内部管理
    /// 保留字段以兼容现有代码引用（均为空 HashMap）
    pub pending_turns: HashMap<String, PendingTurnStub>,
}

/// PendingTurn 占位结构（原 PendingTurn 已移除）
#[derive(Debug)]
pub struct PendingTurnStub {
    pub session_id: String,
}
