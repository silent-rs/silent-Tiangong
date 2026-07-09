//! 文本转语音插件。
//!
//! 端点构造时注入，配置变更时经 on_config_updated 从 config 内存单例热更新。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::tool_override::PromptSectionProvider;
use tiangong_llm::{ModelCapability, ModelEndpoint};

pub struct TextToSpeechPlugin {
    workspace: RwLock<Option<PathBuf>>,
    endpoint: RwLock<ModelEndpoint>,
}

impl TextToSpeechPlugin {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        Self {
            workspace: RwLock::new(None),
            endpoint: RwLock::new(endpoint),
        }
    }

    pub(crate) fn endpoint(&self) -> ModelEndpoint {
        self.endpoint.read().map(|g| g.clone()).unwrap_or_default()
    }
}

impl Plugin for TextToSpeechPlugin {
    fn id(&self) -> &str {
        "text-to-speech"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }

    fn on_config_updated(&self, _config: &tiangong_core::core_config::CoreConfig) {
        if let Some(resolved) =
            tiangong_config::registry::models().resolve_for_capability(ModelCapability::Tts)
        {
            if let Ok(mut guard) = self.endpoint.write() {
                *guard = ModelEndpoint::from_resolved(resolved);
            }
        }
    }
}

impl PromptSectionProvider for TextToSpeechPlugin {}
