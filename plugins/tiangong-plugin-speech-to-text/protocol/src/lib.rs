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
pub const RECORD_START_OPERATION: &str = "record_start";
pub const RECORD_STOP_OPERATION: &str = "record_stop";
pub const RECORD_CANCEL_OPERATION: &str = "record_cancel";

pub struct Transcribe;
pub struct RecordStart;
pub struct RecordStop;
pub struct RecordCancel;

impl SttOperation for Transcribe {
    const NAME: &'static str = TRANSCRIBE_OPERATION;
    type Request = TranscribeRequest;
    type Response = TranscribeResponse;
}

impl SttOperation for RecordStart {
    const NAME: &'static str = RECORD_START_OPERATION;
    type Request = RecordStartRequest;
    type Response = RecordStartResponse;
}

impl SttOperation for RecordStop {
    const NAME: &'static str = RECORD_STOP_OPERATION;
    type Request = Empty;
    type Response = RecordStopResponse;
}

impl SttOperation for RecordCancel {
    const NAME: &'static str = RECORD_CANCEL_OPERATION;
    type Request = Empty;
    type Response = Empty;
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
    /// 转录音频文件路径（回传请求的 file_path，供前端关联语音消息回放）。
    pub audio_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Empty {}

/// 开始录音请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordStartRequest {
    /// 目标采样率（Hz），默认 16000。
    #[serde(default)]
    pub sample_rate: Option<u32>,
}

/// 开始录音响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordStartResponse {
    /// 录音会话 ID（用于停止录音）。
    pub session_id: String,
}

/// 停止录音响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordStopResponse {
    /// 录音音频文件路径。
    pub file_path: String,
    /// 音频 MIME 类型。
    pub mime_type: String,
    /// 录音时长（秒）。
    #[serde(default)]
    pub duration: Option<f64>,
}
