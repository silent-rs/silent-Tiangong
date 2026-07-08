//! 附件分析插件结构体定义与生命周期实现。
//!
//! [`AnalyzeAttachmentPlugin`] 在构造时接收已解析的 multimodal 客户端私有持有，
//! 供 handler 按需调用。客户端由 app 层从 `ModelsConfig` 路由解析后注入，
//! 不再依赖 core runtime 的 `register` 注入。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::model::SingleProviderClient;
use tiangong_core::tool_override::PromptSectionProvider;

/// 附件分析插件。
pub struct AnalyzeAttachmentPlugin {
    /// 当前会话工作目录（由 core 注入，附件分析当前未强依赖，保持一致性预留）。
    workspace: RwLock<Option<PathBuf>>,
    /// 构造时注入的 multimodal 客户端，供 handler 按需调用。
    client: Option<SingleProviderClient>,
    /// 状态反馈通道（转发 multimodal 调用的 token 用量，由 set_feedback_tx 注入）。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
}

impl AnalyzeAttachmentPlugin {
    /// 构造插件实例：接收 app 层解析的 multimodal 客户端（None 表示不启用）。
    pub fn new(client: Option<SingleProviderClient>) -> Self {
        Self {
            workspace: RwLock::new(None),
            client,
            feedback_tx: RwLock::new(None),
        }
    }

    /// 取 multimodal 客户端的克隆快照（供 handler 使用，也作为 tool_specs 防御兜底）。
    pub(crate) fn client(&self) -> Option<SingleProviderClient> {
        self.client.clone()
    }

    /// 读取反馈通道的 clone（供 handler 发 token 用量用）。
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
}

// 附件分析工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for AnalyzeAttachmentPlugin {}
