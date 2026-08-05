//! Generate-Image 插件私有业务协议。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PLUGIN_ID: &str = "generate-image";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const IMAGE_PROTOCOL_VERSION: u32 = 1;

pub const TOOL_GENERATE_IMAGE: &str = "generate_image";

pub trait ImageOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

pub const GENERATE_OPERATION: &str = "generate";
pub const LIST_MODELS_OPERATION: &str = "list_models";

pub struct Generate;
pub struct ListModels;

impl ImageOperation for Generate {
    const NAME: &'static str = GENERATE_OPERATION;
    type Request = GenerateRequest;
    type Response = GenerateResponse;
}

impl ImageOperation for ListModels {
    const NAME: &'static str = LIST_MODELS_OPERATION;
    type Request = Empty;
    type Response = ListModelsResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Empty {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub prompt: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub style: Option<String>,
}

/// 单张生成图片的归档结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneratedImage {
    /// 本地归档路径或原始引用（归档失败时为远程 URL/base64）。
    pub reference: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerateResponse {
    /// 归档后的图片列表。
    pub images: Vec<GeneratedImage>,
    /// 实际使用的模型名。
    pub model: String,
}

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
