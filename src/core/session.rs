use chrono::Local;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub messages: Vec<Message>,
    pub created_at: String,
    pub updated_at: String,
}

impl Session {
    pub fn new(title: impl Into<String>) -> Self {
        let now = now_text();
        Self {
            id: new_id(),
            title: title.into(),
            messages: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn append_message(&mut self, role: MessageRole, content: impl Into<String>) {
        self.messages.push(Message {
            id: new_id(),
            role,
            content: content.into(),
            created_at: now_text(),
        });
        self.updated_at = now_text();
    }

    pub fn recent_messages(&self, limit: usize) -> Vec<Message> {
        if self.messages.len() <= limit {
            return self.messages.clone();
        }
        self.messages[self.messages.len() - limit..].to_vec()
    }
}

pub fn now_text() -> String {
    Local::now().naive_local().to_string()
}

fn new_id() -> String {
    scru128::new().to_string()
}
