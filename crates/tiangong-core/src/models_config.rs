use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{ModelProviderConfig, ProviderProtocol};

// ---------------------------------------------------------------------------
// 核心类型
// ---------------------------------------------------------------------------

/// 模型能力枚举 — 描述模型具备什么能力
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    Chat,
    Multimodal,
    ImageGeneration,
    VideoGeneration,
    Stt,
    Tts,
    /// 向量嵌入模型（Memory / 语义检索）
    Embedding,
    /// 重排模型（Memory / 召回精排）
    Rerank,
}

/// 路由槽位枚举 — 描述哪个模型负责什么任务
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingSlot {
    Chat,
    Lite,
    Multimodal,
    ImageGeneration,
    VideoGeneration,
    Stt,
    Tts,
    Embedding,
    Rerank,
}

impl RoutingSlot {
    pub fn key(&self) -> &'static str {
        match self {
            RoutingSlot::Chat => "chat",
            RoutingSlot::Lite => "lite",
            RoutingSlot::Multimodal => "multimodal",
            RoutingSlot::ImageGeneration => "image_generation",
            RoutingSlot::VideoGeneration => "video_generation",
            RoutingSlot::Stt => "stt",
            RoutingSlot::Tts => "tts",
            RoutingSlot::Embedding => "embedding",
            RoutingSlot::Rerank => "rerank",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "chat" => Some(RoutingSlot::Chat),
            "lite" => Some(RoutingSlot::Lite),
            "multimodal" => Some(RoutingSlot::Multimodal),
            "image_generation" => Some(RoutingSlot::ImageGeneration),
            "video_generation" => Some(RoutingSlot::VideoGeneration),
            "stt" => Some(RoutingSlot::Stt),
            "tts" => Some(RoutingSlot::Tts),
            "embedding" => Some(RoutingSlot::Embedding),
            "rerank" => Some(RoutingSlot::Rerank),
            _ => None,
        }
    }

    pub fn all() -> &'static [RoutingSlot] {
        &[
            RoutingSlot::Chat,
            RoutingSlot::Lite,
            RoutingSlot::Multimodal,
            RoutingSlot::ImageGeneration,
            RoutingSlot::VideoGeneration,
            RoutingSlot::Stt,
            RoutingSlot::Tts,
            RoutingSlot::Embedding,
            RoutingSlot::Rerank,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            RoutingSlot::Chat => "对话",
            RoutingSlot::Lite => "轻量文本",
            RoutingSlot::Multimodal => "多模态",
            RoutingSlot::ImageGeneration => "图片生成",
            RoutingSlot::VideoGeneration => "视频生成",
            RoutingSlot::Stt => "语音识别",
            RoutingSlot::Tts => "语音合成",
            RoutingSlot::Embedding => "向量嵌入",
            RoutingSlot::Rerank => "结果重排",
        }
    }

    /// 路由槽位对应的模型能力
    pub fn capability(&self) -> Option<ModelCapability> {
        match self {
            RoutingSlot::Chat => Some(ModelCapability::Chat),
            RoutingSlot::Lite => None, // Lite 是路由槽位，不对应模型能力
            RoutingSlot::Multimodal => Some(ModelCapability::Multimodal),
            RoutingSlot::ImageGeneration => Some(ModelCapability::ImageGeneration),
            RoutingSlot::VideoGeneration => Some(ModelCapability::VideoGeneration),
            RoutingSlot::Stt => Some(ModelCapability::Stt),
            RoutingSlot::Tts => Some(ModelCapability::Tts),
            RoutingSlot::Embedding => Some(ModelCapability::Embedding),
            RoutingSlot::Rerank => Some(ModelCapability::Rerank),
        }
    }

    /// 从模型能力获取对应的路由槽位
    pub fn from_capability(cap: ModelCapability) -> Self {
        match cap {
            ModelCapability::Chat => RoutingSlot::Chat,
            ModelCapability::Multimodal => RoutingSlot::Multimodal,
            ModelCapability::ImageGeneration => RoutingSlot::ImageGeneration,
            ModelCapability::VideoGeneration => RoutingSlot::VideoGeneration,
            ModelCapability::Stt => RoutingSlot::Stt,
            ModelCapability::Tts => RoutingSlot::Tts,
            ModelCapability::Embedding => RoutingSlot::Embedding,
            ModelCapability::Rerank => RoutingSlot::Rerank,
        }
    }
}

impl ModelCapability {
    /// 配置键（snake_case）
    pub fn key(&self) -> &'static str {
        match self {
            ModelCapability::Chat => "chat",
            ModelCapability::Multimodal => "multimodal",
            ModelCapability::ImageGeneration => "image_generation",
            ModelCapability::VideoGeneration => "video_generation",
            ModelCapability::Stt => "stt",
            ModelCapability::Tts => "tts",
            ModelCapability::Embedding => "embedding",
            ModelCapability::Rerank => "rerank",
        }
    }

    /// 从配置键解析能力
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "chat" => Some(ModelCapability::Chat),
            "multimodal" => Some(ModelCapability::Multimodal),
            "image_generation" => Some(ModelCapability::ImageGeneration),
            "video_generation" => Some(ModelCapability::VideoGeneration),
            "stt" => Some(ModelCapability::Stt),
            "tts" => Some(ModelCapability::Tts),
            "embedding" => Some(ModelCapability::Embedding),
            "rerank" => Some(ModelCapability::Rerank),
            _ => None,
        }
    }

    /// 返回所有能力的列表
    pub fn all() -> &'static [ModelCapability] {
        &[
            ModelCapability::Chat,
            ModelCapability::Multimodal,
            ModelCapability::ImageGeneration,
            ModelCapability::VideoGeneration,
            ModelCapability::Stt,
            ModelCapability::Tts,
            ModelCapability::Embedding,
            ModelCapability::Rerank,
        ]
    }

    /// 返回能力的显示名称
    pub fn display_name(&self) -> &'static str {
        match self {
            ModelCapability::Chat => "对话",
            ModelCapability::Multimodal => "多模态",
            ModelCapability::ImageGeneration => "图片生成",
            ModelCapability::VideoGeneration => "视频生成",
            ModelCapability::Stt => "语音识别",
            ModelCapability::Tts => "语音合成",
            ModelCapability::Embedding => "向量嵌入",
            ModelCapability::Rerank => "结果重排",
        }
    }

    /// 返回用于意图分类的标签（大写英文）
    pub fn intent_label(&self) -> &'static str {
        match self {
            ModelCapability::Chat => "SIMPLE",
            ModelCapability::Multimodal => "MULTIMODAL",
            ModelCapability::ImageGeneration => "IMAGE",
            ModelCapability::VideoGeneration => "VIDEO",
            ModelCapability::Stt => "STT",
            ModelCapability::Tts => "TTS",
            ModelCapability::Embedding => "EMBEDDING",
            ModelCapability::Rerank => "RERANK",
        }
    }

    /// 返回意图分类的描述
    pub fn intent_description(&self) -> &'static str {
        match self {
            ModelCapability::Chat => {
                "简单对话（问候、闲聊、知识问答、翻译、解释概念等不需要执行工具或命令的请求）"
            }
            ModelCapability::Multimodal => "多模态请求（需要理解图片、音频等多种输入形式）",
            ModelCapability::ImageGeneration => "图片生成请求（用户要求生成、绘制、创作图片）",
            ModelCapability::VideoGeneration => "视频生成请求（用户要求生成、制作视频）",
            ModelCapability::Stt => "语音识别请求（用户要求将语音/音频转为文字）",
            ModelCapability::Tts => "语音合成请求（用户要求将文字转为语音/朗读）",
            ModelCapability::Embedding => {
                "向量嵌入模型（Memory 语义检索、向量索引等内部任务，不参与意图路由）"
            }
            ModelCapability::Rerank => "结果重排模型（Memory 召回精排等内部任务，不参与意图路由）",
        }
    }

    /// 除 Chat 外的多媒体能力列表（用于意图分类的动态加载）
    pub fn media_capabilities() -> &'static [ModelCapability] {
        &[
            ModelCapability::ImageGeneration,
            ModelCapability::VideoGeneration,
            ModelCapability::Stt,
            ModelCapability::Tts,
            ModelCapability::Multimodal,
        ]
    }
}

/// Provider 连接配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: String, // 支持 ${ENV_VAR} 引用
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub protocol: ProviderProtocol,
}

fn default_timeout_ms() -> u64 {
    60_000
}

/// 单个模型配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub capabilities: Vec<ModelCapability>,
    #[serde(default = "default_options")]
    pub options: Value,
}

impl Default for ModelEntry {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            capabilities: vec![],
            options: default_options(),
        }
    }
}

fn default_options() -> Value {
    Value::Object(serde_json::Map::new())
}

/// 两层模型配置：Provider + Routing
///
/// routing 直接存储 ModelEntry，不再需要中间的 models 映射层。
/// 支持向后兼容：旧格式 routing 值为字符串（引用 models 中的 key），
/// 新格式 routing 值为 ModelEntry 对象。
#[derive(Debug, Clone, Serialize, Default)]
pub struct ModelsConfig {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// 模型注册表 — 存储所有已定义的模型，routing 从中选择
    #[serde(default)]
    pub models: HashMap<String, ModelEntry>,
    #[serde(default)]
    pub routing: HashMap<RoutingSlot, ModelEntry>,
}

/// 向后兼容的反序列化：支持旧格式（routing 值为字符串引用 models）和新格式（routing 值为 ModelEntry）
impl<'de> serde::Deserialize<'de> for ModelsConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            providers: HashMap<String, ProviderConfig>,
            #[serde(default)]
            models: HashMap<String, ModelEntry>,
            #[serde(default)]
            routing: HashMap<RoutingSlot, RawRoutingValue>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawRoutingValue {
            Key(String),
            Entry(ModelEntry),
        }

        let raw = Raw::deserialize(deserializer)?;

        let routing: HashMap<RoutingSlot, ModelEntry> = raw
            .routing
            .into_iter()
            .map(|(slot, val)| {
                let entry = match val {
                    RawRoutingValue::Key(key) => {
                        raw.models.get(&key).cloned().unwrap_or_else(|| {
                            tracing::warn!("路由引用了不存在的模型：{key}");
                            ModelEntry {
                                provider: String::new(),
                                model: key.clone(),
                                capabilities: vec![],
                                options: default_options(),
                            }
                        })
                    }
                    RawRoutingValue::Entry(entry) => entry,
                };
                (slot, entry)
            })
            .collect();

        Ok(ModelsConfig {
            providers: raw.providers,
            models: raw.models,
            routing,
        })
    }
}

/// 解析后的完整模型配置（Provider + Model 合并）
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub provider: String,
    pub base_url: String,
    pub api_key: String, // 已解析环境变量
    pub timeout_ms: u64,
    pub protocol: ProviderProtocol,
    pub model: String,
    pub options: Value,
}

// ---------------------------------------------------------------------------
// 路径工具
// ---------------------------------------------------------------------------

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    None
}

fn storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

fn models_config_path() -> PathBuf {
    storage_root().join("models.json")
}

// ---------------------------------------------------------------------------
// 实现
// ---------------------------------------------------------------------------

impl ModelsConfig {
    /// 检查指定能力是否已配置可用
    /// 对于 Multimodal，除了检查独立路由外，也检查 chat 模型是否自带此能力
    pub fn has_capability(&self, capability: ModelCapability) -> bool {
        let slot = RoutingSlot::from_capability(capability);
        if self.routing.contains_key(&slot) {
            return true;
        }
        // chat 模型自带 multimodal 能力时，视为多模态可用
        if capability == ModelCapability::Multimodal && self.chat_is_multimodal() {
            return true;
        }
        false
    }

    /// 判断 chat 路由指向的模型是否应直接处理图片（跳过 analyze_attachment 工具）
    /// 以模型定义中声明的 capabilities 为准
    pub fn chat_is_multimodal(&self) -> bool {
        let Some(entry) = self.routing.get(&RoutingSlot::Chat) else {
            return false;
        };
        entry.capabilities.contains(&ModelCapability::Multimodal)
    }

    /// 返回当前已配置可用的能力列表
    pub fn available_capabilities(&self) -> Vec<ModelCapability> {
        ModelCapability::all()
            .iter()
            .copied()
            .filter(|cap| self.has_capability(*cap))
            .collect()
    }

    /// 从 ~/.tiangong/models.json 加载，不存在则返回空配置
    pub fn load() -> Self {
        let path = models_config_path();
        if !path.exists() {
            return Self::default();
        }
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// 保存到 ~/.tiangong/models.json
    pub fn save(&self) -> Result<()> {
        let path = models_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        }
        let content =
            serde_json::to_string_pretty(self).with_context(|| "序列化 ModelsConfig 失败")?;
        fs::write(&path, content)
            .with_context(|| format!("写入 models.json 失败：{}", path.display()))?;
        Ok(())
    }

    /// 从旧版 ModelProviderConfig 迁移
    pub fn from_legacy(legacy: &ModelProviderConfig) -> Self {
        let mut providers = HashMap::new();
        let mut routing = HashMap::new();

        let provider = ProviderConfig {
            base_url: legacy.api_base_url.clone(),
            api_key: legacy.api_auth_token.clone(),
            timeout_ms: legacy.api_timeout_ms.parse::<u64>().unwrap_or(60_000),
            protocol: legacy.api_protocol,
        };
        providers.insert("default".to_string(), provider);

        let model_name = if legacy.api_model.is_empty() {
            "default-chat".to_string()
        } else {
            legacy.api_model.clone()
        };

        routing.insert(
            RoutingSlot::Chat,
            ModelEntry {
                provider: "default".to_string(),
                model: model_name,
                capabilities: vec![ModelCapability::Chat],
                options: default_options(),
            },
        );

        let lite = legacy.lite_model();
        if !lite.is_empty() && lite != legacy.api_model {
            routing.insert(
                RoutingSlot::Lite,
                ModelEntry {
                    provider: "default".to_string(),
                    model: lite.to_string(),
                    capabilities: vec![ModelCapability::Chat],
                    options: default_options(),
                },
            );
        }

        Self {
            providers,
            models: HashMap::new(),
            routing,
        }
    }

    /// 从 LlmConfig 构建兼容的 ModelsConfig
    ///
    /// 将扁平的 LlmConfig 端点映射为 Provider + Routing 结构。
    /// 当多个能力使用相同端点时，合并 capabilities 到已有的路由条目。
    pub fn from_llm_config(llm: &crate::core_config::LlmConfig) -> Self {
        let mut providers: HashMap<String, ProviderConfig> = HashMap::new();
        let mut routing: HashMap<RoutingSlot, ModelEntry> = HashMap::new();

        // 跟踪已注册的端点签名，用于合并相同端点的能力
        let mut seen: HashMap<(String, String), RoutingSlot> = HashMap::new();

        let mut register = |name: &str,
                            endpoint: &crate::core_config::ModelEndpoint,
                            slot: RoutingSlot,
                            cap: ModelCapability| {
            let provider_key = format!("{name}-provider");
            providers.insert(
                provider_key.clone(),
                ProviderConfig {
                    base_url: endpoint.base_url.clone(),
                    api_key: endpoint.api_key.clone(),
                    timeout_ms: endpoint.timeout_ms,
                    protocol: endpoint.protocol,
                },
            );

            let sig = (endpoint.base_url.clone(), endpoint.model.clone());

            if let Some(&first_slot) = seen.get(&sig) {
                // 相同端点已注册 — 合并能力到已有条目
                if let Some(entry) = routing.get_mut(&first_slot)
                    && !entry.capabilities.contains(&cap)
                {
                    entry.capabilities.push(cap);
                }
                // 为当前槽位创建带完整能力的条目
                let merged_caps = routing
                    .get(&first_slot)
                    .map(|e| e.capabilities.clone())
                    .unwrap_or_default();
                routing.insert(
                    slot,
                    ModelEntry {
                        provider: provider_key,
                        model: endpoint.model.clone(),
                        capabilities: merged_caps,
                        options: endpoint.options.clone(),
                    },
                );
            } else {
                routing.insert(
                    slot,
                    ModelEntry {
                        provider: provider_key,
                        model: endpoint.model.clone(),
                        capabilities: vec![cap],
                        options: endpoint.options.clone(),
                    },
                );
                seen.insert(sig, slot);
            }
        };

        // Chat（必须）
        register("chat", &llm.chat, RoutingSlot::Chat, ModelCapability::Chat);

        // 可选能力
        if let Some(ref ep) = llm.image_generation {
            register(
                "image",
                ep,
                RoutingSlot::ImageGeneration,
                ModelCapability::ImageGeneration,
            );
        }
        if let Some(ref ep) = llm.tts {
            register("tts", ep, RoutingSlot::Tts, ModelCapability::Tts);
        }
        if let Some(ref ep) = llm.stt {
            register("stt", ep, RoutingSlot::Stt, ModelCapability::Stt);
        }
        if let Some(ref ep) = llm.video_generation {
            register(
                "video",
                ep,
                RoutingSlot::VideoGeneration,
                ModelCapability::VideoGeneration,
            );
        }
        if let Some(ref ep) = llm.multimodal {
            register(
                "multimodal",
                ep,
                RoutingSlot::Multimodal,
                ModelCapability::Multimodal,
            );
        }
        if let Some(ref ep) = llm.embedding {
            register(
                "embedding",
                ep,
                RoutingSlot::Embedding,
                ModelCapability::Embedding,
            );
        }
        if let Some(ref ep) = llm.rerank {
            register("rerank", ep, RoutingSlot::Rerank, ModelCapability::Rerank);
        }

        Self {
            providers,
            models: HashMap::new(),
            routing,
        }
    }

    /// 解析 api_key 中的 ${ENV_VAR} 引用
    pub fn resolve_api_key(raw: &str) -> String {
        if let Some(inner) = raw.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
            std::env::var(inner).unwrap_or_default()
        } else {
            raw.to_string()
        }
    }

    /// 获取指定路由槽位的模型名称
    pub fn routed_model(&self, capability: ModelCapability) -> Option<&str> {
        let slot = RoutingSlot::from_capability(capability);
        self.routing.get(&slot).map(|e| e.model.as_str())
    }

    /// 获取指定路由槽位的完整配置（Provider + Model 合并）
    pub fn resolve_for_capability(&self, capability: ModelCapability) -> Option<ResolvedModel> {
        let slot = RoutingSlot::from_capability(capability);
        self.resolve_slot(slot)
    }

    /// 按路由槽位获取完整配置
    pub fn resolve_slot(&self, slot: RoutingSlot) -> Option<ResolvedModel> {
        let entry = self.routing.get(&slot)?;
        let provider = self.providers.get(&entry.provider)?;

        Some(ResolvedModel {
            provider: entry.provider.clone(),
            base_url: provider.base_url.clone(),
            api_key: Self::resolve_api_key(&provider.api_key),
            timeout_ms: provider.timeout_ms,
            protocol: provider.protocol,
            model: entry.model.clone(),
            options: entry.options.clone(),
        })
    }

    /// 检查 chat 能力是否已配置
    pub fn has_chat(&self) -> bool {
        self.has_capability(ModelCapability::Chat)
    }

    /// 检查配置是否为空（无 provider 也无 routing）
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty() && self.routing.is_empty()
    }

    /// 更新 chat 路由的模型名称，保留其他字段不变
    pub fn update_chat_model(&mut self, model: String) {
        if let Some(entry) = self.routing.get_mut(&RoutingSlot::Chat) {
            entry.model = model;
        } else {
            self.routing.insert(
                RoutingSlot::Chat,
                ModelEntry {
                    provider: "default".to_string(),
                    model,
                    capabilities: vec![ModelCapability::Chat],
                    options: default_options(),
                },
            );
        }
    }

    /// 从 chat routing 生成 ModelProviderConfig（内部用于构建 SingleProviderClient）
    pub fn to_chat_provider_config(&self) -> ModelProviderConfig {
        if let Some(resolved) = self.resolve_for_capability(ModelCapability::Chat) {
            ModelProviderConfig {
                api_auth_token: resolved.api_key,
                api_base_url: resolved.base_url,
                api_timeout_ms: resolved.timeout_ms.to_string(),
                api_protocol: resolved.protocol,
                api_model: resolved.model,
                api_lite_model: String::new(),
            }
        } else {
            ModelProviderConfig::from_env()
        }
    }

    /// 从 lite routing 生成 ModelProviderConfig（未配置时回退到 chat）
    pub fn to_lite_provider_config(&self) -> ModelProviderConfig {
        if let Some(resolved) = self.resolve_slot(RoutingSlot::Lite) {
            ModelProviderConfig {
                api_auth_token: resolved.api_key,
                api_base_url: resolved.base_url,
                api_timeout_ms: resolved.timeout_ms.to_string(),
                api_protocol: resolved.protocol,
                api_model: resolved.model.clone(),
                api_lite_model: resolved.model,
            }
        } else {
            // 回退到 chat 配置
            self.to_chat_provider_config()
        }
    }
}
