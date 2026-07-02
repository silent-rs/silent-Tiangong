//! 文本转语音插件结构体定义与生命周期实现。
//!
//! [`TextToSpeechPlugin`] 通过 [`Plugin::register`] 从 [`RuntimeEngine`] 获取
//! [`ModelsConfig`]（克隆一份私有持有）并据此判定 TTS 能力是否已配置，
//! 写入内部 [`AtomicBool`]。core 的 `register_plugin` 编排保证 `register` 在
//! `tool_specs` 收集之前执行，故 `tool_specs` 读到的能力开关已是最新值。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::media;
use tiangong_core::models_config::ModelsConfig;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::tool_override::PromptSectionProvider;

/// 文本转语音插件。
pub struct TextToSpeechPlugin {
    /// 当前会话工作目录（由 core 注入，TTS 默认输出到 ~/.tiangong/media，未强依赖）。
    workspace: RwLock<Option<PathBuf>>,
    /// 克隆自 engine 的模型配置，供 handler 调用 media facade。
    models_config: RwLock<Option<ModelsConfig>>,
    /// TTS 能力是否已配置（register 时据 LlmConfig/ModelsConfig 判定）。
    has_tts: AtomicBool,
}

impl TextToSpeechPlugin {
    /// 构造插件实例：初始无配置，待 `register` 注入。
    pub fn new() -> Self {
        Self {
            workspace: RwLock::new(None),
            models_config: RwLock::new(None),
            has_tts: AtomicBool::new(false),
        }
    }

    /// 取 models_config 的克隆快照（供 handler 使用）。
    pub(crate) fn models_config(&self) -> Option<ModelsConfig> {
        self.models_config.read().ok()?.clone()
    }

    /// TTS 能力是否已配置。
    pub(crate) fn has_tts(&self) -> bool {
        self.has_tts.load(Ordering::Relaxed)
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
        let models = engine.models_config().clone();
        let llm = engine.llm_config();
        self.has_tts
            .store(media::has_tts(&models, llm), Ordering::Relaxed);
        if let Ok(mut guard) = self.models_config.write() {
            *guard = Some(models);
        }
    }
}

// 文本转语音工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for TextToSpeechPlugin {}
