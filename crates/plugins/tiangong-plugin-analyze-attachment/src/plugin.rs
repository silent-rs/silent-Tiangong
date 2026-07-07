//! 附件分析插件结构体定义与生命周期实现。
//!
//! [`AnalyzeAttachmentPlugin`] 通过 [`Plugin::register`] 从 [`RuntimeEngine`] 克隆
//! multimodal 客户端私有持有，供 handler 按需调用。
//!
//! 注册模式与其他媒体插件一致：入口层经 [`crate::should_register`] 判断能力是否存在、
//! 满足条件才注册本插件。插件内部不再维护 `enabled` 标志，`tool_specs()` 以
//! `client().is_some()` 作为防御兜底（入口层已保证满足条件时才注册）。

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
    /// 状态反馈通道（转发 multimodal 调用的 token 用量，由 set_feedback_tx 注入）。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
}

impl AnalyzeAttachmentPlugin {
    /// 构造插件实例：初始无配置，待 `register` 注入。
    pub fn new() -> Self {
        Self {
            workspace: RwLock::new(None),
            client: RwLock::new(None),
            feedback_tx: RwLock::new(None),
        }
    }

    /// 取 multimodal 客户端的克隆快照（供 handler 使用，也作为 tool_specs 防御兜底）。
    pub(crate) fn client(&self) -> Option<SingleProviderClient> {
        self.client.read().ok()?.clone()
    }

    /// 读取反馈通道的 clone（供 handler 发 token 用量用）。
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

    fn register(&self, engine: &RuntimeEngine) {
        // 入口层已通过 should_register 保证满足条件才注册；此处保留防御性判定，
        // 仅当配置了 multimodal 客户端、且对话模型本身非 multimodal 时才缓存 client。
        // engine 重建后 multimodal 配置变更时也会清空旧 client。
        if let Ok(mut guard) = self.client.write() {
            *guard = if engine.has_multimodal_client() && !engine.chat_is_multimodal() {
                Some(engine.multimodal_client().clone())
            } else {
                None
            };
        }
    }
}

// 附件分析工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for AnalyzeAttachmentPlugin {}
