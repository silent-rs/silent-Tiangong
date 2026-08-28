//! Text-To-Speech 插件私有业务协议。
//!
//! 纯数据 + 操作 marker，不依赖核心库，可同时编译 native + wasm32。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PLUGIN_ID: &str = "text-to-speech";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const TTS_PROTOCOL_VERSION: u32 = 1;

/// 工具名常量。
pub const TOOL_TEXT_TO_SPEECH: &str = "text_to_speech";

/// 一个类型化 TTS 业务操作。
pub trait TtsOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

pub const SYNTHESIZE_OPERATION: &str = "synthesize";
pub const LIST_MODELS_OPERATION: &str = "list_models";
pub const LIST_VOICES_OPERATION: &str = "list_voices";
pub const PLAY_OPERATION: &str = "play";
pub const PLAY_STATUS_OPERATION: &str = "play_status";
pub const STOP_OPERATION: &str = "stop";

pub struct Synthesize;
pub struct ListModels;
pub struct ListVoices;
pub struct Play;
pub struct PlayStatus;
pub struct Stop;

impl TtsOperation for Synthesize {
    const NAME: &'static str = SYNTHESIZE_OPERATION;
    type Request = SynthesizeRequest;
    type Response = SynthesizeResponse;
}

impl TtsOperation for ListModels {
    const NAME: &'static str = LIST_MODELS_OPERATION;
    type Request = Empty;
    type Response = ListModelsResponse;
}

impl TtsOperation for ListVoices {
    const NAME: &'static str = LIST_VOICES_OPERATION;
    type Request = Empty;
    type Response = ListVoicesResponse;
}

impl TtsOperation for Play {
    const NAME: &'static str = PLAY_OPERATION;
    type Request = PlayRequest;
    type Response = PlayResponse;
}

impl TtsOperation for PlayStatus {
    const NAME: &'static str = PLAY_STATUS_OPERATION;
    type Request = Empty;
    type Response = PlayStatusResponse;
}

impl TtsOperation for Stop {
    const NAME: &'static str = STOP_OPERATION;
    type Request = Empty;
    type Response = Empty;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Empty {}

/// 语音合成请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SynthesizeRequest {
    /// 待合成文本。
    pub text: String,
    /// 音色（可选，未指定时由 sidecar 从模型配置 fallback）。
    #[serde(default)]
    pub voice: Option<String>,
    /// 语速（可选）。
    #[serde(default)]
    pub speed: Option<f64>,
}

/// 语音合成响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SynthesizeResponse {
    /// 音频文件本地路径。
    pub file_path: String,
    /// 音频 MIME 类型。
    pub mime_type: String,
    /// 音频时长（秒，可能不返回）。
    #[serde(default)]
    pub duration: Option<f64>,
    /// 实际使用的模型名。
    pub model: String,
}

/// 脱敏模型信息（设置页用，不含 API Key）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelInfo {
    pub key: String,
    pub provider: String,
    pub model: String,
    pub configured: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListModelsResponse {
    pub models: Vec<ModelInfo>,
}

/// 播放音频请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayRequest {
    /// 音频文件本地路径。
    pub file_path: String,
}

/// 播放音频响应。
///
/// 播放为后台执行：请求返回只表示「已启动」，完成状态经
/// [`PLAY_STATUS_OPERATION`] 轮询获取。这样播放期间 sidecar 仍可响应
/// stop 等其他请求（stdio 分发是串行的，阻塞式播放会让 stop 永远排队）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayResponse {
    /// 是否成功启动播放。
    pub started: bool,
}

/// 播放状态响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayStatusResponse {
    /// 是否有音频正在播放。
    pub playing: bool,
}

/// 音色信息（设置页选择用）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub gender: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListVoicesResponse {
    pub voices: Vec<VoiceInfo>,
}
