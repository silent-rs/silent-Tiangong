use super::super::*;

#[derive(Debug)]
pub struct RuntimeState {
    pub run: RunSnapshot,
    pub pending_turns: HashMap<String, PendingTurn>,
}
