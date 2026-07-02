//! 语音转文本插件结构体定义与生命周期实现。
//!
//! [`SpeechToTextPlugin`] 通过 [`Plugin::register`] 从 [`RuntimeEngine`] 获取
//! [`ModelsConfig`]（克隆一份私有持有）并据此判定 STT 能力是否已配置，
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

/// 语音转文本插件。
pub struct SpeechToTextPlugin {
    /// 当前会话工作目录（由 core 注入，STT 当前未强依赖，保持一致性预留）。
    workspace: RwLock<Option<PathBuf>>,
    /// 克隆自 engine 的模型配置，供 handler 调用 media facade。
    models_config: RwLock<Option<ModelsConfig>>,
    /// STT 能力是否已配置（register 时据 LlmConfig/ModelsConfig 判定）。
    has_stt: AtomicBool,
}

impl SpeechToTextPlugin {
    /// 构造插件实例：初始无配置，待 `register` 注入。
    pub fn new() -> Self {
        Self {
            workspace: RwLock::new(None),
            models_config: RwLock::new(None),
            has_stt: AtomicBool::new(false),
        }
    }

    /// 取 models_config 的克隆快照（供 handler 使用）。
    pub(crate) fn models_config(&self) -> Option<ModelsConfig> {
        self.models_config.read().ok()?.clone()
    }

    /// STT 能力是否已配置。
    pub(crate) fn has_stt(&self) -> bool {
        self.has_stt.load(Ordering::Relaxed)
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

    fn set_workspace(&self, workspace: &std::path::Path) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = Some(workspace.to_path_buf());
        }
    }

    fn register(&self, engine: &RuntimeEngine) {
        let models = engine.models_config().clone();
        let llm = engine.llm_config();
        self.has_stt
            .store(media::has_stt(&models, llm), Ordering::Relaxed);
        if let Ok(mut guard) = self.models_config.write() {
            *guard = Some(models);
        }
    }
}

// 语音转文本工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for SpeechToTextPlugin {}
