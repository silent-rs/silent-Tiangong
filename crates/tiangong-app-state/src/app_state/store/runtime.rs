use super::super::*;

#[derive(Debug)]
pub struct RuntimeState {
    pub run: RunSnapshot,
    /// 当前正在执行 turn 的会话索引，用于快照和 Desktop 后台完成通知。
    /// 实际执行仍由 TiangongCore 管理，这里只保存轻量 session_id。
    pub pending_turns: HashMap<String, PendingTurnStub>,
}

/// PendingTurn 占位结构（原 PendingTurn 已移除）
#[derive(Debug)]
pub struct PendingTurnStub {
    pub session_id: String,
}
