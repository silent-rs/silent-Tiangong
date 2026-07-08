//! 语音转文本插件结构体定义与生命周期实现。
//!
//! [`SpeechToTextPlugin`] 在构造时接收已解析的 STT 端点私有持有，供 handler
//! 调用 media facade。端点由 app 层从 `ModelsConfig` 路由解析后注入。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::tool_override::PromptSectionProvider;

/// 语音转文本插件。
pub struct SpeechToTextPlugin {
    /// 当前会话工作目录（由 core 注入，STT 当前未强依赖，保持一致性预留）。
    workspace: RwLock<Option<PathBuf>>,
    /// 构造时注入的 STT 模型端点配置，供 handler 调用 media facade。
    endpoint: Option<ModelEndpoint>,
}

impl SpeechToTextPlugin {
    /// 构造插件实例：接收 app 层解析的端点（None 表示能力未配置，插件不生效）。
    pub fn new(endpoint: Option<ModelEndpoint>) -> Self {
        Self {
            workspace: RwLock::new(None),
            endpoint,
        }
    }

    /// 取 endpoint 的克隆快照（供 handler 使用）。
    pub(crate) fn endpoint(&self) -> Option<ModelEndpoint> {
        self.endpoint.clone()
    }
}

impl Plugin for SpeechToTextPlugin {
    fn id(&self) -> &str {
        "speech-to-text"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }
}

// 语音转文本工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for SpeechToTextPlugin {}
