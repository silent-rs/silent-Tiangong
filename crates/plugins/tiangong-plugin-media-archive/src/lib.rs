//! 媒体归档插件（media-archive）。
//!
//! 接管全部媒体归档职责，使 core 不再直接依赖 `tiangong-media-archive`：
//! - 用户输入附件归档（`on_message_ingress`）；
//! - 工具输出图片本地化（`on_tool_result_localize`）。
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
