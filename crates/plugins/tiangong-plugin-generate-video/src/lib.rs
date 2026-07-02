//! 视频生成进程内插件（`generate_video`）。
//!
//! 通过 OpenAI 兼容 API（异步任务 + 轮询）生成视频。[`ModelsConfig`] 在 engine 创建时
//! 由 [`Plugin::register`] 注入，插件据此在 [`Plugin::tool_specs`] 中按能力是否配置
//! 决定是否向 LLM 暴露工具。判定逻辑（LlmConfig 优先、ModelsConfig 回退）与原 core
//! `inject_enhanced_tools` 完全一致，迁移自 `runtime.rs`。

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
