use super::super::*;

#[derive(Debug)]
pub struct SessionState {
    pub sessions: Vec<Session>,
    pub active_session_id: String,
    pub workspace_dir: String,
    pub session_title_draft: String,
    pub input_draft: String,
}
