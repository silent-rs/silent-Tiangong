use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use tiangong_core::core::Plugin;
use tiangong_types::{MentionContext, MentionGroup, MentionQuery, MentionRequest, MentionTarget};

use super::CoreManager;

impl CoreManager {
    /// App 注册/更新查询句柄，不改变已有 Core 的工具声明。
    pub fn set_mention_plugins(&self, plugins: Vec<Arc<dyn Plugin>>) {
        *self
            .mention_plugins
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = plugins;
    }

    /// 不创建 Core、不执行 turn；调用者应在阻塞线程执行插件查询。
    pub fn query_mentions(&self, request: MentionRequest) -> Result<Vec<MentionGroup>, String> {
        let context = match request.target {
            MentionTarget::Global => MentionContext::default(),
            MentionTarget::Draft { workspace } => MentionContext {
                session_id: None,
                workspace: Some(normalize_workspace(&workspace)?),
            },
            MentionTarget::Session { session_id } => {
                let metadata =
                    crate::SessionMetadata::load_from_storage(&self.storage_root, &session_id)?;
                MentionContext {
                    session_id: Some(session_id),
                    workspace: if metadata.cwd.trim().is_empty() {
                        None
                    } else {
                        Some(normalize_workspace(&metadata.cwd)?)
                    },
                }
            }
        };
        let query = MentionQuery {
            context,
            query: request.query,
            allowed_kinds: request.allowed_kinds,
            max_per_group: request.max_per_group.min(1000),
        };
        if query.max_per_group == 0 {
            return Ok(Vec::new());
        }
        let plugins = self
            .mention_plugins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut groups: BTreeMap<String, MentionGroup> = BTreeMap::new();
        let mut seen = HashSet::new();
        let needle = query.query.to_lowercase();
        for plugin in plugins {
            let candidates = match plugin.query_mentions(&query) {
                Ok(candidates) => candidates,
                Err(error) => {
                    tracing::warn!(plugin_id = plugin.id(), %error, "mention 查询失败");
                    continue;
                }
            };
            for candidate in candidates {
                if !query.allowed_kinds.is_empty() && !query.allowed_kinds.contains(&candidate.kind)
                {
                    continue;
                }
                if !needle.is_empty()
                    && ![&candidate.label, &candidate.value, &candidate.hint]
                        .iter()
                        .any(|value| value.to_lowercase().contains(&needle))
                {
                    continue;
                }
                if !seen.insert((candidate.kind.clone(), candidate.value.clone())) {
                    continue;
                }
                let group = groups
                    .entry(candidate.kind.clone())
                    .or_insert_with(|| MentionGroup {
                        kind: candidate.kind.clone(),
                        label: candidate.kind.clone(),
                        candidates: Vec::new(),
                    });
                if group.candidates.len() < query.max_per_group {
                    group.candidates.push(candidate);
                }
            }
        }
        Ok(groups.into_values().collect())
    }
}

fn normalize_workspace(workspace: &str) -> Result<String, String> {
    let path = Path::new(workspace);
    if !path.is_absolute() {
        return Err("工作区必须是绝对路径".into());
    }
    let path = path
        .canonicalize()
        .map_err(|e| format!("无法解析工作区: {e}"))?;
    if !path.is_dir() {
        return Err("工作区不是目录".into());
    }
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiangong_core::config::core::{CoreConfig, CoreConfigProvider};
    use tiangong_core::tools::extension::{
        MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
    };
    use tiangong_types::MentionCandidate;

    struct FakePlugin;
    impl ToolSpecProvider for FakePlugin {}
    impl ToolOverrideHandler for FakePlugin {}
    impl PromptSectionProvider for FakePlugin {}
    impl MentionCandidateProvider for FakePlugin {
        fn mention_candidates(&self) -> Vec<MentionCandidate> {
            (0..1500)
                .map(|i| MentionCandidate {
                    value: format!("@file:{i}"),
                    label: format!("file-{i}"),
                    kind: "file".into(),
                    ..Default::default()
                })
                .collect()
        }
    }
    impl Plugin for FakePlugin {
        fn id(&self) -> &str {
            "fake"
        }
    }

    #[test]
    fn injected_plugin_is_visible_before_core_creation() {
        let root = tempfile::tempdir().unwrap();
        let manager = CoreManager::new(CoreConfigProvider::new(CoreConfig::default()), root.path());
        manager.set_mention_plugins(vec![Arc::new(FakePlugin)]);
        let groups = manager
            .query_mentions(MentionRequest {
                query: "1499".into(),
                max_per_group: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(groups[0].candidates[0].value, "@file:1499");
        assert!(manager.cores.lock().unwrap().is_empty());
        assert!(!root.path().join("sessions").exists());
        manager.set_mention_plugins(Vec::new());
        assert!(
            manager
                .query_mentions(MentionRequest::default())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invalid_session_never_falls_back_to_global() {
        let root = tempfile::tempdir().unwrap();
        let manager = CoreManager::new(CoreConfigProvider::new(CoreConfig::default()), root.path());
        manager.set_mention_plugins(vec![Arc::new(FakePlugin)]);
        for session_id in ["missing", "../escape"] {
            assert!(
                manager
                    .query_mentions(MentionRequest {
                        target: MentionTarget::Session {
                            session_id: session_id.into()
                        },
                        ..Default::default()
                    })
                    .is_err()
            );
        }
    }
}
