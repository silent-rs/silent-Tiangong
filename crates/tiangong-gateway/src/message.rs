use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingMessage {
    pub id: String,
    pub connector: String,
    pub channel_id: String,
    pub sender_id: String,
    pub content: MessageContent,
    pub reply_to: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingMessage {
    pub content: MessageContent,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageContent {
    Text(String),
    Image {
        url: String,
        caption: Option<String>,
    },
    File {
        url: String,
        name: String,
    },
    Audio {
        url: String,
        duration: Option<u32>,
    },
    Video {
        url: String,
        caption: Option<String>,
    },
}
