//! 语音转文本进程内插件。
//!
//! 入口层（app 层）负责判断是否注册并解析端点，构造时注入。插件本身不做注册判定，
//! 只持有端点供 handler 调用后端。

pub mod handler;
pub mod plugin;

pub use plugin::SpeechToTextPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;
use tiangong_llm::ModelEndpoint;

/// 构造插件实例，接收 app 层已解析的端点。
pub fn build_plugin(endpoint: ModelEndpoint) -> Arc<dyn Plugin> {
    Arc::new(SpeechToTextPlugin::new(endpoint))
}
