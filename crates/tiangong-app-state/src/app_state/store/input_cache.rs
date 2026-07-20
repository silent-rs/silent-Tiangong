use serde::{Deserialize, Serialize};

/// 输入框缓存。已有会话以 Session ID 为键；新对话以预留的未来 Session ID 为键。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InputCache {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<tiangong_media_archive::RawAttachment>,
    #[serde(default)]
    pub is_sending: bool,
    #[serde(default)]
    pub revision: u64,
}
