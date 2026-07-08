//! 语音转文本插件结构体定义与生命周期实现。
//!
//! [`SpeechToTextPlugin`] 通过 [`Plugin::register`] 从 [`RuntimeEngine`] 的
//! [`ModelsConfig`] 路由解析 STT 端点，转换为 [`ModelEndpoint`] 私有持有，
//! 供 handler 调用 media facade。端点不再经 `LlmConfig` 字段中转。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::models_config::ModelCapability;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::tool_override::PromptSectionProvider;

/// 语音转文本插件。
pub struct SpeechToTextPlugin {
    /// 当前会话工作目录（由 core 注入，STT 当前未强依赖，保持一致性预留）。
    workspace: RwLock<Option<PathBuf>>,
    /// 从 ModelsConfig 路由解析的 STT 模型端点配置，供 handler 调用 media facade。
    endpoint: RwLock<Option<ModelEndpoint>>,
}

impl SpeechToTextPlugin {
    /// 构造插件实例：初始无配置，待 `register` 注入。
    pub fn new() -> Self {
        Self {
            workspace: RwLock::new(None),
            endpoint: RwLock::new(None),
        }
    }

    /// 取 endpoint 的克隆快照（供 handler 使用）。
    pub(crate) fn endpoint(&self) -> Option<ModelEndpoint> {
        self.endpoint.read().ok()?.clone()
    }
}

impl Default for SpeechToTextPlugin {
    fn default() -> Self {
        Self::new()
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

    fn register(&self, engine: &RuntimeEngine) {
        if let Some(resolved) = engine
            .models_config()
            .resolve_for_capability(ModelCapability::Stt)
        {
            if let Ok(mut guard) = self.endpoint.write() {
                *guard = Some(ModelEndpoint::from_resolved(resolved));
            }
        }
    }
}

// 语音转文本工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for SpeechToTextPlugin {}
