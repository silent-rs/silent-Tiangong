use super::super::*;

#[derive(Debug)]
pub(in crate::core::app_state) struct RuntimeState {
    pub(in crate::core::app_state) run: RunSnapshot,
    pub(in crate::core::app_state) pending_turn: Option<PendingTurn>,
}
