//! Speech-To-Text 插件私有业务协议。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PLUGIN_ID: &str = "speech-to-text";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const STT_PROTOCOL_VERSION: u32 = 1;

pub const TOOL_SPEECH_TO_TEXT: &str = "speech_to_text";

pub trait SttOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

pub const TRANSCRIBE_OPERATION: &str = "transcribe";

pub struct Transcribe;

impl SttOperation for Transcribe {
    const NAME: &'static str = TRANSCRIBE_OPERATION;
    type Request = TranscribeRequest;
    type Response = TranscribeResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscribeRequest {
    /// 音频文件路径（仅允许 ~/.tiangong/media/ 目录下）。
    pub file_path: String,
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscribeResponse {
    pub text: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    pub model: String,
}
