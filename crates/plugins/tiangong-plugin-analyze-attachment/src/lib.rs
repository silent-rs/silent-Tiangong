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
use tiangong_core::core_config::LlmConfig;

/// 判断入口层是否应注册附件分析插件。
///
/// 与原 runtime `inject_enhanced_tools` 的注入条件、以及 `RuntimeEngine` 的 multimodal
/// fallback 判定保持一致：仅当配置了 multimodal 端点、且 chat 主模型本身非 multimodal
/// 时才需要本工具（否则附件直接随主模型请求发送）。判断基于 [`ModelsConfig`] 重建，
/// 与 engine 的 `chat_is_multimodal()` 同源，避免入口层与 engine 判断不一致。
///
/// 入口层（CLI / Server / Tauri）统一调用本函数，避免三处复制复杂判断。
pub fn should_register(llm: &LlmConfig) -> bool {
    let models_config = tiangong_core::models_config::ModelsConfig::from_llm_config(llm);
    llm.multimodal.is_some() && !models_config.chat_is_multimodal()
}

/// 构造附件分析插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(AnalyzeAttachmentPlugin::new())
}

/// 构造默认的附件分析插件列表，供各入口（CLI / Server / Tauri）注入 core 时使用。
///
/// 入口层应先用 [`should_register`] 判断能力是否存在，满足条件才调用本函数。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
