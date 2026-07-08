//! 视频生成插件结构体定义与生命周期实现。
//!
//! [`GenerateVideoPlugin`] 在构造时接收已解析的视频生成端点私有持有，供 handler
//! 调用 media facade。端点由 app 层从 `ModelsConfig` 路由解析后注入。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::tool_override::PromptSectionProvider;
use tiangong_llm::ModelEndpoint;

/// 视频生成插件。
pub struct GenerateVideoPlugin {
    /// 当前会话工作目录（由 core 注入，视频生成当前未强依赖，保持一致性预留）。
    workspace: RwLock<Option<PathBuf>>,
    /// 构造时注入的视频生成端点，供 handler 调用 media facade。
    endpoint: ModelEndpoint,
}

impl GenerateVideoPlugin {
    /// 构造插件实例：接收 app 层已解析的端点。
    pub fn new(endpoint: ModelEndpoint) -> Self {
        Self {
            workspace: RwLock::new(None),
            endpoint,
        }
    }

    /// 取 endpoint 的克隆快照（供 handler 使用）。
    pub(crate) fn endpoint(&self) -> Option<ModelEndpoint> {
        Some(self.endpoint.clone())
    }
}

impl Plugin for GenerateVideoPlugin {
    fn id(&self) -> &str {
        "generate-video"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }
}

// 视频生成工具无需注入 Prompt 段落，使用空实现满足 supertrait 约束。
impl PromptSectionProvider for GenerateVideoPlugin {}
