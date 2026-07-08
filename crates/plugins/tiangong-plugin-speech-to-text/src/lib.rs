//! 语音转文本进程内插件（`speech_to_text`）。
//!
//! 通过 OpenAI 兼容 API（Whisper）转录音频。入口层构造插件时从 `ModelsConfig`
//! 解析 STT 能力对应的端点注入；插件私有持有，供 handler 直接调用后端。
//! 不再依赖 core runtime 注入。

pub mod handler;
pub mod plugin;

pub use plugin::SpeechToTextPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;
use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::models_config::{ModelCapability, ModelsConfig};

/// 构造语音转文本插件实例，接收已解析的端点（None 表示能力未配置，插件不生效）。
pub fn build_plugin(endpoint: Option<ModelEndpoint>) -> Arc<dyn Plugin> {
    Arc::new(SpeechToTextPlugin::new(endpoint))
}

/// 构造默认的语音转文本插件列表：从 ModelsConfig 解析能力，未配置时返回空 Vec。
pub fn default_plugins(models: &ModelsConfig) -> Vec<Arc<dyn Plugin>> {
    let endpoint = models
        .resolve_for_capability(ModelCapability::Stt)
        .map(ModelEndpoint::from_resolved);
    if endpoint.is_some() {
        vec![build_plugin(endpoint)]
    } else {
        Vec::new()
    }
}
