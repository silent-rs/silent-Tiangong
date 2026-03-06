use super::super::*;

#[derive(Debug)]
pub(in crate::core::app_state) struct SessionState {
    pub(in crate::core::app_state) sessions: Vec<Session>,
    pub(in crate::core::app_state) active_session_id: String,
    pub(in crate::core::app_state) session_title_draft: String,
    pub(in crate::core::app_state) input_draft: String,
}
