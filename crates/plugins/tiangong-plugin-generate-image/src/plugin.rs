//! 图片生成插件结构体定义与生命周期实现。
//!
//! 端点构造时注入，配置变更时经 `on_config_updated` 从 config 内存单例取最新
//! models 路由解析热更新——无需重建 engine/core。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::tool_override::PromptSectionProvider;
use tiangong_llm::{ModelCapability, ModelEndpoint};

/// 图片生成插件。
pub struct GenerateImagePlugin {
    workspace: RwLock<Option<PathBuf>>,
    endpoint: RwLock<ModelEndpoint>,
}

impl GenerateImagePlugin {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        Self {
            workspace: RwLock::new(None),
            endpoint: RwLock::new(endpoint),
        }
    }

    pub(crate) fn endpoint(&self) -> ModelEndpoint {
        self.endpoint.read().map(|g| g.clone()).unwrap_or_default()
    }
}

impl Plugin for GenerateImagePlugin {
    fn id(&self) -> &str {
        "generate-image"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }

    fn on_config_updated(&self, _config: &tiangong_core::core_config::CoreConfig) {
        if let Some(resolved) = tiangong_config::registry::models()
            .resolve_for_capability(ModelCapability::ImageGeneration)
        {
            if let Ok(mut guard) = self.endpoint.write() {
                *guard = ModelEndpoint::from_resolved(resolved);
            }
        }
    }
}

impl PromptSectionProvider for GenerateImagePlugin {
    fn prompt_sections(&self) -> Vec<String> {
        // 仅在已配置有效端点时注入能力说明，遵循「能力拥有者提供」。
        let ep = self.endpoint();
        if ep.base_url.is_empty() || ep.model.is_empty() {
            return Vec::new();
        }
        vec![format!("图片生成：已配置（模型：{}）", ep.model)]
    }
}
