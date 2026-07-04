//! 图片生成进程内插件（`generate_image`）。
//!
//! 通过 OpenAI 兼容 API（DALL·E / gpt-image-1 等，具体 provider 由配置决定）生成图片。
//! 入口层根据 [`LlmConfig`] 的图片生成能力配置决定是否注册本插件；注册后，
//! [`Plugin::register`] 从 engine 取出对应的 [`ModelEndpoint`] 私有持有，供 handler
//! 直接调用后端（免去每次路由解析）。

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
