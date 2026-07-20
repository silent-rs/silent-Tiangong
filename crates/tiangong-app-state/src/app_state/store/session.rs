use serde::{Deserialize, Serialize};

/// 宿主 App 按会话维护的输入草稿。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionInputDraft {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<tiangong_media_archive::RawAttachment>,
    #[serde(default)]
    pub is_sending: bool,
    #[serde(default)]
    pub revision: u64,
}
