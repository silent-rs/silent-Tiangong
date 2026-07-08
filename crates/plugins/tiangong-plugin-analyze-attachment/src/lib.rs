//! 附件分析插件（analyze_attachment）。
//!
//! 将原 runtime `inject_enhanced_tools` 中对 `analyze_attachment` 的特判注入与
//! `core::execute_attachment_analysis_tool` 的分发逻辑收敛为独立插件 crate。
//!
//! 注册模式与其他媒体插件（generate-image / generate-video / text-to-speech /
//! speech-to-text）一致：入口层根据能力配置条件注册（见 [`should_register`]），
//! 插件内部只负责在 `register` 阶段缓存 multimodal client 并提供工具规格。

pub mod handler;
pub mod plugin;

pub use plugin::AnalyzeAttachmentPlugin;

use std::sync::Arc;
use tiangong_core::core::Plugin;
use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::model::SingleProviderClient;
use tiangong_core::models_config::{ModelCapability, ModelsConfig};

/// 判断入口层是否应注册附件分析插件。
///
/// 仅当配置了独立 multimodal 路由、且 chat 主模型本身非 multimodal 时才需要本工具
/// （否则附件直接随主模型请求发送）。
pub fn should_register(models: &ModelsConfig) -> bool {
    models.has_capability(ModelCapability::Multimodal) && !models.chat_is_multimodal()
}

/// 构造附件分析插件实例，接收已解析的 multimodal 客户端（None 表示不启用）。
pub fn build_plugin(client: Option<SingleProviderClient>) -> Arc<dyn Plugin> {
    Arc::new(AnalyzeAttachmentPlugin::new(client))
}

/// 构造默认的附件分析插件列表：从 ModelsConfig 解析 multimodal 能力，
/// 仅当配置了独立 multimodal 路由、且 chat 非 multimodal 时才注册（内部含 should_register 判定）。
pub fn default_plugins(models: &ModelsConfig) -> Vec<Arc<dyn Plugin>> {
    if !should_register(models) {
        return Vec::new();
    }
    let client = models
        .resolve_for_capability(ModelCapability::Multimodal)
        .map(|resolved| SingleProviderClient::new(ModelEndpoint::from_resolved(resolved)));
    vec![build_plugin(client)]
}
