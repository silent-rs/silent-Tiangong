use super::super::formatting::{
    build_turn_conclusion, format_llm_output_message, format_plan_snapshot,
    format_tool_trace_message, merge_tool_result_text, summarize_verify_for_result,
    workspace_change_overview,
};
use super::super::repository::{elapsed_ms_u64, new_scru128_string};
use super::super::*;

mod completion;
mod start;
mod stream;

#[derive(Debug, Clone, Copy, Default)]
pub struct AppTurnService;

impl AppTurnService {
    fn find_message_mut<'a>(
        self,
        state: &'a mut TiangongState,
        session_id: &str,
        message_id: &str,
    ) -> Option<&'a mut Message> {
        state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)?
            .messages
            .iter_mut()
            .find(|msg| msg.id == message_id)
    }
}
