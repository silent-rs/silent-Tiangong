use std::collections::HashSet;

/// PendingTurn 占位结构。
#[derive(Debug)]
pub struct PendingTurnStub {
    pub session_id: String,
    pub queued_message_ids: HashSet<String>,
    pub accepted_message_ids: HashSet<String>,
    pub legacy_pending: bool,
}
