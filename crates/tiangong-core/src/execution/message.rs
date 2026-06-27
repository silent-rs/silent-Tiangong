use crate::session::{ContentBlock, Message, MessagePhase, MessageRole, now_text};

pub(crate) fn runtime_message(role: MessageRole, content: impl Into<String>) -> Message {
    Message {
        id: scru128::new().to_string(),
        role,
        content: vec![ContentBlock::text(content.into())],
        reasoning_content: String::new(),
        reasoning_signature: None,
        worker_id: None,
        media: Vec::new(),
        media_migrated: true,
        elapsed_ms: None,
        turn_status: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_result_is_error: false,
        compact: false,
        phase: MessagePhase::Normal,
        created_at: now_text(),
    }
}
