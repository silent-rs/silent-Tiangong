//! 附件分析插件结构体定义与生命周期实现。
//!
//! multimodal 客户端构造时注入，配置变更时经 `on_config_updated` 从 config 内存
//! 单例取最新 models 路由解析热更新——无需重建 engine/core。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::tool_override::PromptSectionProvider;
use tiangong_llm::{ModelCapability, ModelEndpoint, SingleProviderClient};

/// 附件分析插件。
pub struct AnalyzeAttachmentPlugin {
    workspace: RwLock<Option<PathBuf>>,
    /// multimodal 客户端，构造时注入、配置变更时热更新。
    client: RwLock<SingleProviderClient>,
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
}

impl AnalyzeAttachmentPlugin {
    pub fn new(client: SingleProviderClient) -> Self {
        Self {
            workspace: RwLock::new(None),
            client: RwLock::new(client),
            feedback_tx: RwLock::new(None),
        }
    }

    pub(crate) fn client(&self) -> SingleProviderClient {
        self.client
            .read()
            .map(|g| g.clone())
            .expect("client mutex poisoned")
    }

    pub(crate) fn feedback_tx(&self) -> Option<PluginFeedbackTx> {
        self.feedback_tx.read().ok()?.as_ref().cloned()
    }
}

impl Plugin for AnalyzeAttachmentPlugin {
    fn id(&self) -> &str {
        "analyze-attachment"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }

    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.feedback_tx.write() {
            *guard = Some(tx);
        }
    }

    fn on_config_updated(&self, _config: &tiangong_core::core_config::CoreConfig) {
        let models = tiangong_config::registry::models();
        if !models.chat_is_multimodal() {
            if let Some(resolved) = models.resolve_for_capability(ModelCapability::Multimodal) {
                if let Ok(mut guard) = self.client.write() {
                    *guard = SingleProviderClient::new(ModelEndpoint::from_resolved(resolved));
                }
            }
        }
    }
}

impl PromptSectionProvider for AnalyzeAttachmentPlugin {}
