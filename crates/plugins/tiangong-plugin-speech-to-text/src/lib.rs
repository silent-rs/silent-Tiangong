//! 语音转文本进程内插件（`speech_to_text`）。
//!
//! 通过 OpenAI 兼容 API（Whisper）转录音频。入口层根据 [`LlmConfig`] 的 STT 能力
//! 配置决定是否注册本插件；注册后，[`Plugin::register`] 从 engine 取出对应的
//! [`ModelEndpoint`] 私有持有，供 handler 直接调用后端（免去每次路由解析）。

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
