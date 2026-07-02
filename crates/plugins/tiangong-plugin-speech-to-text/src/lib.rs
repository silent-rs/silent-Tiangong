//! 语音转文本进程内插件（`speech_to_text`）。
//!
//! 通过 OpenAI 兼容 API（Whisper）转录音频。[`ModelsConfig`] 在 engine 创建时
//! 由 [`Plugin::register`] 注入，插件据此在 [`Plugin::tool_specs`] 中按能力是否配置
//! 决定是否向 LLM 暴露工具。判定逻辑（LlmConfig 优先、ModelsConfig 回退）与原 core
//! `inject_enhanced_tools` 完全一致，迁移自 `runtime.rs`。

pub mod handler;
pub mod plugin;

pub use plugin::SpeechToTextPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造语音转文本插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(SpeechToTextPlugin::new())
}

/// 构造默认的语音转文本插件列表，供各入口（CLI / Server）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
