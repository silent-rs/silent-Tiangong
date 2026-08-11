//! Generate-Image-OpenAI 插件私有业务协议。
//!
//! 通过 OpenAI Responses API 的 image_generation 工具生成图片。
//! 支持两种模型来源：全局模型配置（models.json）或手动输入端点。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PLUGIN_ID: &str = "generate-image-openai";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const IMAGE_PROTOCOL_VERSION: u32 = 1;

pub const TOOL_GENERATE_IMAGE: &str = "generate_image";

pub const GENERATE_OPERATION: &str = "generate";
pub const GET_CONFIG_OPERATION: &str = "get_config";
pub const SET_CONFIG_OPERATION: &str = "set_config";
pub const RECONFIGURE_OPERATION: &str = "reconfigure";

/// 一个类型化的 Image 业务操作。
pub trait ImageOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

pub struct Generate;
pub struct GetConfig;
pub struct SetConfig;
pub struct Reconfigure;

impl ImageOperation for Generate {
    const NAME: &'static str = GENERATE_OPERATION;
    type Request = GenerateRequest;
    type Response = GenerateResponse;
}

impl ImageOperation for GetConfig {
    const NAME: &'static str = GET_CONFIG_OPERATION;
    type Request = Empty;
    type Response = ConfigBootstrap;
}

impl ImageOperation for SetConfig {
    const NAME: &'static str = SET_CONFIG_OPERATION;
    type Request = ConfigSelection;
    type Response = Ack;
}

impl ImageOperation for Reconfigure {
    const NAME: &'static str = RECONFIGURE_OPERATION;
    type Request = Empty;
    type Response = Ack;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Empty {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ack {}

/// 模型来源。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// 选择全局模型配置（models.json）。
    #[default]
    Global,
    /// 手动输入端点。
    Manual,
}

impl ModelSource {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Manual => "manual",
        }
    }
}

/// 手动输入的端点信息。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManualEndpoint {
    /// OpenAI 兼容 base_url，如 `https://api.openai.com/v1`。
    pub base_url: String,
    /// API Key，支持 `${ENV_VAR}` 形式的环境变量引用。
    pub api_key: String,
    /// 主模型 id（支持 image_generation 工具的模型），如 `gpt-5.3-codex`。
    pub model: String,
}

/// 已解析的模型端点（保存配置时从 models.json 解析并缓存，运行时不再依赖 models.json）。
///
/// 与 memory 插件的做法一致：保存配置时一次性解析，避免运行时反复读 models.json。
/// 含明文密钥，仅供 sidecar 内部使用，不返回给 UI。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedEndpoint {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl From<&ManualEndpoint> for ResolvedEndpoint {
    fn from(endpoint: &ManualEndpoint) -> Self {
        Self {
            base_url: endpoint.base_url.clone(),
            api_key: endpoint.api_key.clone(),
            model: endpoint.model.clone(),
        }
    }
}

/// 插件持久化配置（存于 `~/.tiangong/generate-image-openai/config.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenConfig {
    /// 模型来源。
    #[serde(default)]
    pub source: ModelSource,
    /// 选择全局模型时对应的 models.json key。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_model_key: Option<String>,
    /// 手动输入端点。
    #[serde(default)]
    pub manual_endpoint: ManualEndpoint,
    /// 已解析并缓存的端点（保存配置时写入，运行时直接使用）。
    #[serde(default)]
    pub resolved: ResolvedEndpoint,
    /// 附加系统提示（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_prompt: Option<String>,
}

impl Default for ImageGenConfig {
    fn default() -> Self {
        Self {
            source: ModelSource::Global,
            global_model_key: None,
            manual_endpoint: ManualEndpoint::default(),
            resolved: ResolvedEndpoint::default(),
            extra_prompt: None,
        }
    }
}

/// UI 保存配置时发送（只含选择，不含 models.json 已有密钥）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigSelection {
    #[serde(default)]
    pub source: ModelSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_model_key: Option<String>,
    #[serde(default)]
    pub manual_endpoint: ManualEndpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_prompt: Option<String>,
}

impl From<&ImageGenConfig> for ConfigSelection {
    fn from(config: &ImageGenConfig) -> Self {
        Self {
            source: config.source.clone(),
            global_model_key: config.global_model_key.clone(),
            manual_endpoint: config.manual_endpoint.clone(),
            extra_prompt: config.extra_prompt.clone(),
        }
    }
}

/// 脱敏模型信息（设置页展示用，不含 API Key）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelInfo {
    /// models.json 中的 key。
    pub key: String,
    pub provider: String,
    pub model: String,
    /// 是否已完成配置（provider 存在且 api_key 非空）。
    pub configured: bool,
}

/// 设置页加载时一次性返回当前配置 + 可选模型列表。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigBootstrap {
    /// 当前已保存的配置。
    pub config: ImageGenConfig,
    /// 全局 chat 能力模型列表（脱敏）。
    pub models: Vec<ModelInfo>,
}

/// 生成请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerateRequest {
    /// 图片描述。
    pub prompt: String,
    /// 要编辑的原图本地路径列表（可选）。
    ///
    /// 传入时进入编辑模式（Responses API 的 `action: edit`），
    /// 不传时为纯生成模式。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

/// 单张生成图片的归档结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneratedImage {
    /// 本地归档路径或原始引用（归档失败时为远程 URL/base64）。
    pub reference: String,
}

/// 生成响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerateResponse {
    /// 归档后的图片列表。
    pub images: Vec<GeneratedImage>,
    /// 实际使用的模型名。
    pub model: String,
}
