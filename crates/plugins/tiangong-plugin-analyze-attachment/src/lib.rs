//! 附件分析插件（analyze_attachment）。
//!
//! 入口层（app 层）负责判断是否注册并解析 multimodal 客户端，构造时注入。插件本身
//! 不做注册判定，只持有 client 供 handler 调用。

pub mod handler;
pub mod plugin;

pub use plugin::AnalyzeAttachmentPlugin;

use std::sync::Arc;
use tiangong_core::core::Plugin;
use tiangong_llm::SingleProviderClient;

/// 构造附件分析插件实例，接收 app 层已解析的 multimodal 客户端。
pub fn build_plugin(client: SingleProviderClient) -> Arc<dyn Plugin> {
    Arc::new(AnalyzeAttachmentPlugin::new(client))
}
