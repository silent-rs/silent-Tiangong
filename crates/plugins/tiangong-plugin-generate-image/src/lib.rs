//! 图片生成进程内插件（`generate_image`）。
//!
//! 通过 OpenAI 兼容 API（DALL·E / gpt-image-1 等，具体 provider 由配置决定）生成图片。
//! 入口层构造插件时从 `ModelsConfig` 解析图片生成能力对应的端点注入；插件私有持有，
//! 供 handler 直接调用后端（免去每次路由解析）。不再依赖 core runtime 注入。

pub mod handler;
pub mod plugin;

pub use plugin::GenerateImagePlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;
use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::models_config::{ModelCapability, ModelsConfig};

/// 构造图片生成插件实例，接收已解析的端点（None 表示能力未配置，插件不生效）。
pub fn build_plugin(endpoint: Option<ModelEndpoint>) -> Arc<dyn Plugin> {
    Arc::new(GenerateImagePlugin::new(endpoint))
}

/// 构造默认的图片生成插件列表：从 ModelsConfig 解析能力，未配置时返回空 Vec。
pub fn default_plugins(models: &ModelsConfig) -> Vec<Arc<dyn Plugin>> {
    let endpoint = models
        .resolve_for_capability(ModelCapability::ImageGeneration)
        .map(ModelEndpoint::from_resolved);
    if endpoint.is_some() {
        vec![build_plugin(endpoint)]
    } else {
        Vec::new()
    }
}
