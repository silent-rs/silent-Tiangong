//! Desktop TypeScript 工具插件的进程内适配器。
//!
//! 适配器只把清单中的工具规格和提示词接入 Core，并把工具调用转交给
//! [`crate::ts_tools`]。工具参数和结果语义完全由插件 TypeScript 实现。

use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{
    MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
};

use crate::manifest::{PluginManifest, TsToolDecl};

#[derive(Clone, Default)]
struct TsPluginState {
    tools: Vec<TsToolDecl>,
    prompts: Vec<String>,
}

pub struct TsPluginAdapter {
    id: String,
    state: RwLock<TsPluginState>,
    enabled: AtomicBool,
}

impl TsPluginAdapter {
    pub(crate) fn from_manifest(manifest: &PluginManifest, enabled: bool) -> Self {
        Self {
            id: manifest.id.clone(),
            state: RwLock::new(TsPluginState {
                tools: manifest.tools.clone().unwrap_or_default(),
                prompts: manifest.prompt.clone().unwrap_or_default(),
            }),
            enabled: AtomicBool::new(enabled),
        }
    }

    pub(crate) fn reconfigure(&self, manifest: &PluginManifest, enabled: bool) {
        let next = TsPluginState {
            tools: manifest.tools.clone().unwrap_or_default(),
            prompts: manifest.prompt.clone().unwrap_or_default(),
        };
        match self.state.write() {
            Ok(mut state) => *state = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
        self.set_enabled(enabled);
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn tool(&self, name: &str) -> Option<TsToolDecl> {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
    }
}

impl Plugin for TsPluginAdapter {
    fn id(&self) -> &str {
        &self.id
    }
}

impl ToolSpecProvider for TsPluginAdapter {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        if !self.is_enabled() {
            return Vec::new();
        }
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tools
            .iter()
            .map(|tool| ToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
            })
            .collect()
    }
}

impl ToolOverrideHandler for TsPluginAdapter {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        _actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
        if !self.is_enabled() {
            return Box::pin(async { None });
        }
        let Some(tool) = self.tool(&call.name) else {
            return Box::pin(async { None });
        };
        let plugin_id = self.id.clone();
        let session_id = session.id.clone();
        let call = call.clone();
        Box::pin(async move {
            Some(crate::ts_tools::execute(plugin_id, session_id, call, tool.timeout_ms).await)
        })
    }
}

impl PromptSectionProvider for TsPluginAdapter {
    fn prompt_sections(&self) -> Vec<String> {
        if !self.is_enabled() {
            return Vec::new();
        }
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .prompts
            .clone()
    }
}

impl MentionCandidateProvider for TsPluginAdapter {}
