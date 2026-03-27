use crate::session::{Message, MessageRole, now_text};

pub(crate) fn runtime_message(role: MessageRole, content: impl Into<String>) -> Message {
    Message {
        id: scru128::new().to_string(),
        role,
        content: content.into(),
        reasoning_content: String::new(),
        created_at: now_text(),
    }
}
