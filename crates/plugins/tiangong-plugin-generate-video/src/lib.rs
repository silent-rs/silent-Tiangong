//! 视频生成进程内插件（`generate_video`）。
//!
//! 通过 OpenAI 兼容 API（异步任务 + 轮询）生成视频。入口层根据 [`LlmConfig`] 的
//! 视频生成能力配置决定是否注册本插件；注册后，[`Plugin::register`] 从 engine 取出
//! 对应的 [`ModelEndpoint`] 私有持有，供 handler 直接调用后端（免去每次路由解析）。

pub mod handler;
pub mod plugin;

pub use plugin::GenerateVideoPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造视频生成插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(GenerateVideoPlugin::new())
}

/// 构造默认的视频生成插件列表，供各入口（CLI / Server）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
