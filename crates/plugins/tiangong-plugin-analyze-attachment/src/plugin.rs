//! 附件分析插件结构体定义与生命周期实现。
//!
//! [`AnalyzeAttachmentPlugin`] 通过 [`Plugin::register`] 从 [`RuntimeEngine`] 克隆
//! multimodal 客户端私有持有，并根据 `has_multimodal_client && !chat_is_multimodal`
//! 设置 `enabled` 标志：`tool_specs()` 仅在启用时返回工具规格。
//!
//! 注入条件与原 runtime `inject_enhanced_tools` 完全一致，区别在于判定收敛到插件
//! 内部，避免在入口层复制 multimodal 回退规则（fallback：当 chat 模型自带 multimodal
//! 但无独立 multimodal 端点时，core 会用 chat client 充当 multimodal_client）。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::model::SingleProviderClient;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::tool_override::PromptSectionProvider;

/// 附件分析插件。
pub struct AnalyzeAttachmentPlugin {
    /// 当前会话工作目录（由 core 注入，附件分析当前未强依赖，保持一致性预留）。
    workspace: RwLock<Option<PathBuf>>,
    /// 克隆自 engine 的 multimodal 客户端，供 handler 按需调用。
    client: RwLock<Option<SingleProviderClient>>,
    /// 是否暴露工具：仅当配置了 multimodal 客户端且对话模型非 multimodal 时为 true。
    enabled: RwLock<bool>,
    /// 状态反馈通道（转发 multimodal 调用的 token 用量，由 set_feedback_tx 注入）。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
}

impl AnalyzeAttachmentPlugin {
    /// 构造插件实例：初始无配置，待 `register` 注入。
    pub fn new() -> Self {
        Self {
            workspace: RwLock::new(None),
            client: RwLock::new(None),
            enabled: RwLock::new(false),
            feedback_tx: RwLock::new(None),
        }
    }

    /// 取 multimodal 客户端的克隆快照（供 handler 使用）。
    pub(crate) fn client(&self) -> Option<SingleProviderClient> {
        self.client.read().ok()?.clone()
    }

    /// 当前是否已启用（register 之后读取）。
    pub(crate) fn enabled(&self) -> bool {
        self.enabled.read().map(|g| *g).unwrap_or(false)
    }

    /// 读取反馈通道的 clone（供 handler 发 token 用量事件用）。
    pub(crate) fn feedback_tx(&self) -> Option<PluginFeedbackTx> {
        self.feedback_tx.read().ok()?.as_ref().cloned()
    }
}

impl Default for AnalyzeAttachmentPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for AnalyzeAttachmentPlugin {
    fn id(&self) -> &str {
        "analyze-attachment"
    }

    fn set_workspace(&self, workspace: &std::path::Path) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = Some(workspace.to_path_buf());
        }
    }

    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.feedback_tx.write() {
            *guard = Some(tx);
        }
    }

    fn register(&self, engine: &RuntimeEngine) {
        // 注入条件与原 runtime::inject_enhanced_tools 一致：
        // 仅当配置了 multimodal 客户端、且对话模型本身非 multimodal 时启用。
        let enabled = engine.has_multimodal_client() && !engine.chat_is_multimodal();
        if let Ok(mut guard) = self.enabled.write() {
            *guard = enabled;
        }
        // enabled=false 时清空旧 client，避免工具未暴露但仍持有陈旧客户端状态
        // （如 engine 重建后 multimodal 配置变更）。
        if let Ok(mut guard) = self.client.write() {
            *guard = if enabled {
                Some(engine.multimodal_client().clone())
            } else {
                None
            };
        }
    }
}

// 附件分析工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for AnalyzeAttachmentPlugin {}
