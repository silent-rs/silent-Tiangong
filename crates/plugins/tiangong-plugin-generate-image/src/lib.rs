//! 图片生成进程内插件（`generate_image`）。
//!
//! 通过 OpenAI 兼容 API（DALL·E / gpt-image-1 等，具体 provider 由配置决定）生成图片。
//! [`ModelsConfig`] 在 engine 创建时由 [`Plugin::register`] 注入，插件据此在
//! [`Plugin::tool_specs`] 中按能力是否配置决定是否向 LLM 暴露工具。
//!
//! 判定逻辑（LlmConfig 优先、ModelsConfig 回退）与原 core `inject_enhanced_tools`
//! 完全一致，迁移自 `runtime.rs`。

pub mod handler;
pub mod plugin;

pub use plugin::GenerateImagePlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造图片生成插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(GenerateImagePlugin::new())
}

/// 构造默认的图片生成插件列表，供各入口（CLI / Server）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
