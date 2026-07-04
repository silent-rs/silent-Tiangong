//! 附件分析插件（analyze_attachment）。
//!
//! 将原 runtime `inject_enhanced_tools` 中对 `analyze_attachment` 的特判注入与
//! `core::execute_attachment_analysis_tool` 的分发逻辑收敛为独立插件 crate。
//!
//! 注入条件保持不变：仅当 engine 配置了 multimodal 客户端且对话模型本身非
//! multimodal 时才暴露工具（否则图片直接随消息发送，无需工具）。

pub mod handler;
pub mod plugin;

pub use plugin::AnalyzeAttachmentPlugin;

use std::sync::Arc;
use tiangong_core::core::Plugin;

/// 构造附件分析插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(AnalyzeAttachmentPlugin::new())
}

/// 构造默认的附件分析插件列表，供各入口（CLI / Server / Tauri）注入 core 时使用。
///
/// 是否真正暴露工具由插件在 `register` 时根据 engine 的 multimodal 状态动态决定，
/// 因此入口层可无条件注册。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
