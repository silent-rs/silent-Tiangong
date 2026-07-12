//! 附件分析插件结构体定义与生命周期实现。
//!
//! multimodal 客户端构造时注入，配置变更时经 `on_config_updated` 从 config 内存
//! 单例取最新 models 路由解析热更新——无需重建 engine/core。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::permission::PermissionLevel;
use tiangong_core::tool_override::PromptSectionProvider;
use tiangong_llm::{ModelCapability, ModelEndpoint, SingleProviderClient};

pub(crate) const TOOL_ANALYZE_ATTACHMENT: &str = "analyze_attachment";

fn permission_overrides() -> std::collections::BTreeMap<String, PermissionLevel> {
    std::collections::BTreeMap::from([(
        TOOL_ANALYZE_ATTACHMENT.to_string(),
        PermissionLevel::Critical,
    )])
}

fn prompt_section() -> String {
    format!(
        "## 附件分析工具\n\
         当用户消息明确列出需要分析的图片资源，且回答确实需要查看图片内容时，可调用 `{TOOL_ANALYZE_ATTACHMENT}`。\n\
         调用时必须使用该用户消息标注的 `message_id`；`attachment_index` 从 0 开始，对应消息中的资源顺序。\n\
         文档和其他文件应使用对应文件工具；普通文本对话、无需查看图片内容或消息未提供可分析图片时，不要调用此工具。"
    )
}

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

    fn tool_permission_overrides(&self) -> std::collections::BTreeMap<String, PermissionLevel> {
        permission_overrides()
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

impl PromptSectionProvider for AnalyzeAttachmentPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        vec![prompt_section()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owns_its_prompt_and_permission_metadata() {
        let prompt = prompt_section();
        assert!(prompt.contains(TOOL_ANALYZE_ATTACHMENT));
        assert!(prompt.contains("message_id"));
        assert!(prompt.contains("attachment_index"));
        assert_eq!(
            permission_overrides().get(TOOL_ANALYZE_ATTACHMENT),
            Some(&PermissionLevel::Critical)
        );
    }
}
