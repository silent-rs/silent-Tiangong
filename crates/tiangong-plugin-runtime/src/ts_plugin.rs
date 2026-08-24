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
    /// @提及展示：候选 label（UI 贡献标题或插件 id）与副标题（mention.hint）。
    mention: Option<(String, String)>,
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
                mention: mention_candidate_parts(manifest),
            }),
            enabled: AtomicBool::new(enabled),
        }
    }

    pub(crate) fn reconfigure(&self, manifest: &PluginManifest, enabled: bool) {
        let next = TsPluginState {
            tools: manifest.tools.clone().unwrap_or_default(),
            prompts: manifest.prompt.clone().unwrap_or_default(),
            mention: mention_candidate_parts(manifest),
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

impl MentionCandidateProvider for TsPluginAdapter {
    fn mention_candidates(&self) -> Vec<tiangong_core::MentionCandidate> {
        if !self.is_enabled() {
            return Vec::new();
        }
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .mention
            .as_ref()
            .map(|(label, hint)| {
                vec![tiangong_core::MentionCandidate {
                    value: format!("@plugin:{}", self.id),
                    label: label.clone(),
                    kind: "plugin".to_string(),
                    hint: hint.clone(),
                }]
            })
            .unwrap_or_default()
    }
}

/// 从清单推导 @提及候选的展示字段：label 取首个 UI 贡献标题（缺省插件 id），
/// hint 取 mention.hint（未声明 mention 则无候选）。
fn mention_candidate_parts(manifest: &PluginManifest) -> Option<(String, String)> {
    let mention = manifest.mention.as_ref()?;
    let label = manifest
        .ui
        .as_ref()
        .and_then(|ui| ui.contributions.first())
        .map(|contribution| {
            if contribution.title.is_empty() {
                manifest.id.clone()
            } else {
                contribution.title.clone()
            }
        })
        .unwrap_or_else(|| manifest.id.clone());
    Some((label, mention.hint.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MentionManifest;

    fn manifest_with_mention(hint: Option<&str>) -> PluginManifest {
        let mention = hint.map(|hint| {
            serde_json::from_str::<MentionManifest>(&format!(r#"{{"hint":"{hint}"}}"#)).unwrap()
        });
        PluginManifest {
            schema_version: 2,
            id: "demo".into(),
            version: "0.1.0".into(),
            wasm: None,
            sidecar: None,
            permissions: vec![],
            entrypoints: None,
            model_requirements: None,
            storage_access: false,
            capabilities: None,
            ui: Some(serde_json::from_str(
                r#"{"contributions":[{"slot":"extension.tab","id":"app","title":"演示插件","entry":"app/index.html"}]}"#,
            )
            .unwrap()),
            tools: None,
            prompt: None,
            resources: None,
            mention,
        }
    }

    #[test]
    fn mention候选_声明时生成_禁用时为空() {
        let adapter =
            TsPluginAdapter::from_manifest(&manifest_with_mention(Some("问候能力")), true);
        let candidates = MentionCandidateProvider::mention_candidates(&adapter);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "@plugin:demo");
        assert_eq!(candidates[0].label, "演示插件");
        assert_eq!(candidates[0].kind, "plugin");
        assert_eq!(candidates[0].hint, "问候能力");

        adapter.set_enabled(false);
        assert!(MentionCandidateProvider::mention_candidates(&adapter).is_empty());

        // 未声明 mention：无候选
        let adapter = TsPluginAdapter::from_manifest(&manifest_with_mention(None), true);
        assert!(MentionCandidateProvider::mention_candidates(&adapter).is_empty());
    }

    #[test]
    fn mention候选_无ui标题时用插件id() {
        let mut manifest = manifest_with_mention(Some("能力"));
        manifest.ui = None;
        let adapter = TsPluginAdapter::from_manifest(&manifest, true);
        let candidates = MentionCandidateProvider::mention_candidates(&adapter);
        assert_eq!(candidates[0].label, "demo");
    }
}
