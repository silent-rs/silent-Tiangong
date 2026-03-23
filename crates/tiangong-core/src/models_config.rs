use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::ModelProviderConfig;

// ---------------------------------------------------------------------------
// 核心类型
// ---------------------------------------------------------------------------

/// 模型能力枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    Chat,
    Multimodal,
    ImageGeneration,
    VideoGeneration,
    Stt,
    Tts,
}

impl ModelCapability {
    /// 返回所有能力的列表
    pub fn all() -> &'static [ModelCapability] {
        &[
            ModelCapability::Chat,
            ModelCapability::Multimodal,
            ModelCapability::ImageGeneration,
            ModelCapability::VideoGeneration,
            ModelCapability::Stt,
            ModelCapability::Tts,
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
        }
    }
}

/// Provider 连接配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: String, // 支持 ${ENV_VAR} 引用
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
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

fn default_options() -> Value {
    Value::Object(serde_json::Map::new())
}

/// 三层模型配置：Provider + Model + Routing
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelsConfig {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub models: HashMap<String, ModelEntry>,
    #[serde(default)]
    pub routing: HashMap<ModelCapability, String>,
}

/// 解析后的完整模型配置（Provider + Model 合并）
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub base_url: String,
    pub api_key: String, // 已解析环境变量
    pub timeout_ms: u64,
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
        let content = serde_json::to_string_pretty(self)
            .with_context(|| "序列化 ModelsConfig 失败")?;
        fs::write(&path, content)
            .with_context(|| format!("写入 models.json 失败：{}", path.display()))?;
        Ok(())
    }

    /// 从旧版 ModelProviderConfig 迁移
    ///
    /// 将旧的单 provider 配置转为一个 "default" provider + 一个 chat model + routing
    pub fn from_legacy(legacy: &ModelProviderConfig) -> Self {
        let mut providers = HashMap::new();
        let mut models = HashMap::new();
        let mut routing = HashMap::new();

        // 构建 default provider
        let provider = ProviderConfig {
            base_url: legacy.api_base_url.clone(),
            api_key: legacy.api_auth_token.clone(),
            timeout_ms: legacy.api_timeout_ms.parse::<u64>().unwrap_or(60_000),
        };
        providers.insert("default".to_string(), provider);

        // 构建 chat model
        let model_name = if legacy.api_model.is_empty() {
            "default-chat".to_string()
        } else {
            legacy.api_model.clone()
        };

        let model_entry = ModelEntry {
            provider: "default".to_string(),
            model: legacy.api_model.clone(),
            capabilities: vec![ModelCapability::Chat],
            options: default_options(),
        };
        models.insert(model_name.clone(), model_entry);

        // 如果有 lite model，也添加
        let lite = legacy.lite_model();
        if !lite.is_empty() && lite != legacy.api_model {
            let lite_entry = ModelEntry {
                provider: "default".to_string(),
                model: lite.to_string(),
                capabilities: vec![ModelCapability::Chat],
                options: default_options(),
            };
            models.insert(lite.to_string(), lite_entry);
        }

        // 设置 routing
        routing.insert(ModelCapability::Chat, model_name);

        Self {
            providers,
            models,
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

    /// 获取指定能力的路由模型名称
    pub fn routed_model(&self, capability: ModelCapability) -> Option<&str> {
        self.routing.get(&capability).map(|s| s.as_str())
    }

    /// 获取指定能力的完整配置（Provider + Model 合并）
    pub fn resolve_for_capability(&self, capability: ModelCapability) -> Option<ResolvedModel> {
        let model_name = self.routing.get(&capability)?;
        let model_entry = self.models.get(model_name)?;
        let provider = self.providers.get(&model_entry.provider)?;

        Some(ResolvedModel {
            base_url: provider.base_url.clone(),
            api_key: Self::resolve_api_key(&provider.api_key),
            timeout_ms: provider.timeout_ms,
            model: model_entry.model.clone(),
            options: model_entry.options.clone(),
        })
    }

    /// 检查 chat 能力是否已配置
    pub fn has_chat(&self) -> bool {
        self.resolve_for_capability(ModelCapability::Chat).is_some()
    }

    /// 检查配置是否为空（无 provider 也无 model）
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty() && self.models.is_empty()
    }
}
