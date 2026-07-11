//! 媒体归档插件（media-archive）。
//!
//! 接管工具输出图片本地化（`on_tool_result_localize`），使 core 不再直接
//! 依赖 `tiangong-media-archive`。
//!
//! 输入附件归档由各入口层在 deliver 前完成，不经此插件。
//!
//! 归档是基础能力，由各入口无条件注册。

pub mod plugin;

pub use plugin::MediaArchivePlugin;

use std::sync::Arc;
use tiangong_core::core::Plugin;

/// 构造媒体归档插件实例。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(MediaArchivePlugin::new())
}
