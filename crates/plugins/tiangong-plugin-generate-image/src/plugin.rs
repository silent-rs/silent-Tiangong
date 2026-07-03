//! 图片生成插件结构体定义与生命周期实现。
//!
//! [`GenerateImagePlugin`] 通过 [`Plugin::register`] 从 [`RuntimeEngine`] 克隆
//! [`ModelsConfig`] 私有持有，供 handler 调用 media facade。是否注册本插件由入口层
//! 根据 [`LlmConfig`] 的能力配置决定（未配置图片生成能力则不注册）。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::models_config::ModelsConfig;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::tool_override::PromptSectionProvider;

/// 图片生成插件。
pub struct GenerateImagePlugin {
    /// 当前会话工作目录（由 core 注入，图片生成当前未强依赖，保持一致性预留）。
    workspace: RwLock<Option<PathBuf>>,
    /// 克隆自 engine 的模型配置，供 handler 调用 media facade。
    models_config: RwLock<Option<ModelsConfig>>,
}

impl GenerateImagePlugin {
    /// 构造插件实例：初始无配置，待 `register` 注入。
    pub fn new() -> Self {
        Self {
            workspace: RwLock::new(None),
            models_config: RwLock::new(None),
        }
    }

    /// 取 models_config 的克隆快照（供 handler 使用）。
    pub(crate) fn models_config(&self) -> Option<ModelsConfig> {
        self.models_config.read().ok()?.clone()
    }
}

impl Default for GenerateImagePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for GenerateImagePlugin {
    fn id(&self) -> &str {
        "generate-image"
    }

    fn set_workspace(&self, workspace: &std::path::Path) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = Some(workspace.to_path_buf());
        }
    }

    fn register(&self, engine: &RuntimeEngine) {
        if let Ok(mut guard) = self.models_config.write() {
            *guard = Some(engine.models_config().clone());
        }
    }
}

// 图片生成工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for GenerateImagePlugin {}
