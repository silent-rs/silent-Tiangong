//! 图片生成插件结构体定义与生命周期实现。
//!
//! [`GenerateImagePlugin`] 通过 [`Plugin::register`] 从 [`RuntimeEngine`] 的
//! [`LlmConfig`] 中克隆图片生成对应的 [`ModelEndpoint`] 私有持有，供 handler 调用
//! media facade。是否注册本插件由入口层根据 [`LlmConfig`] 的能力配置决定（未配置
//! 图片生成能力则不注册）。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::tool_override::PromptSectionProvider;

/// 图片生成插件。
pub struct GenerateImagePlugin {
    /// 当前会话工作目录（由 core 注入，图片生成当前未强依赖，保持一致性预留）。
    workspace: RwLock<Option<PathBuf>>,
    /// 克隆自 engine 的图片生成端点，供 handler 调用 media facade。
    endpoint: RwLock<Option<ModelEndpoint>>,
}

impl GenerateImagePlugin {
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

impl Default for GenerateImagePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for GenerateImagePlugin {
    fn id(&self) -> &str {
        "generate-image"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }

    fn register(&self, engine: &RuntimeEngine) {
        if let Some(endpoint) = engine.llm_config().and_then(|c| c.image_generation.clone()) {
            if let Ok(mut guard) = self.endpoint.write() {
                *guard = Some(endpoint);
            }
        }
    }
}

// 图片生成工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for GenerateImagePlugin {}
