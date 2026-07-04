//! 文本转语音插件结构体定义与生命周期实现。
//!
//! [`TextToSpeechPlugin`] 通过 [`Plugin::register`] 从 [`RuntimeEngine`] 克隆
//! [`ModelEndpoint`] 私有持有，供 handler 调用 media facade。是否注册本插件由入口层
//! 根据 [`LlmConfig`] 的能力配置决定（未配置文本转语音能力则不注册）。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::tool_override::PromptSectionProvider;

/// 文本转语音插件。
pub struct TextToSpeechPlugin {
    /// 当前会话工作目录（由 core 注入，TTS 默认输出到 ~/.tiangong/media，未强依赖）。
    workspace: RwLock<Option<PathBuf>>,
    /// 克隆自 engine 的 TTS 模型端点，供 handler 调用 media facade。
    endpoint: RwLock<Option<ModelEndpoint>>,
}

impl TextToSpeechPlugin {
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

impl Default for TextToSpeechPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for TextToSpeechPlugin {
    fn id(&self) -> &str {
        "text-to-speech"
    }

    fn set_workspace(&self, workspace: &std::path::Path) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = Some(workspace.to_path_buf());
        }
    }

    fn register(&self, engine: &RuntimeEngine) {
        if let Some(endpoint) = engine.llm_config().and_then(|c| c.tts.clone()) {
            if let Ok(mut guard) = self.endpoint.write() {
                *guard = Some(endpoint);
            }
        }
    }
}

// 文本转语音工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for TextToSpeechPlugin {}
