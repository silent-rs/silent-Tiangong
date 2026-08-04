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

pub struct Synthesize;
pub struct ListModels;

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
