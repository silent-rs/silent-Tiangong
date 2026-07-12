use super::super::*;

/// 宿主 App 按会话维护的输入草稿。
///
/// `is_sending` 仅表示当前进程中的准备/投递状态，不持久化；进程重启后统一恢复为
/// false。`revision` 每次文字或附件变化时递增，用于阻止迟到回调清理新输入。
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

#[derive(Debug)]
pub struct SessionState {
    pub sessions: Vec<Session>,
    pub active_session_id: String,
    pub workspace_dir: String,
    pub session_title_draft: String,
    pub input_drafts: HashMap<String, SessionInputDraft>,
}
