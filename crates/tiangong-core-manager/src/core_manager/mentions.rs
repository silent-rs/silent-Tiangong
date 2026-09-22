//! @提及候选收集（#550 阶段 A）。
//!
//! CoreManager 负责编排：解析查询目标与会话上下文，遍历宿主注入的候选源，
//! 汇总去重分组。具体插件发现与调用由 Runtime 实现，本层不依赖它。
//!
//! 与 Core 的关系：**不经 Core 的 `Plugin`/`MentionCandidateProvider`**。
//! 候选查询不需要会话 Core 存在，也不占用插件位；Core 只管执行 turn。

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use tiangong_types::{
    MentionCandidate, MentionContext, MentionGroup, MentionQuery, MentionRequest, MentionTarget,
};

use super::CoreManager;

/// 候选来源：由宿主注入实现（Runtime 按当前注册表实时查询）。
///
/// 实现需自行处理插件启停、换代与失效保护；查询可能耗时，调用方在阻塞
/// 线程执行。
pub trait MentionSource: Send + Sync + 'static {
    /// 来源标识（日志用）。
    fn id(&self) -> &str;

    /// 按本次查询上下文返回候选。失败不应影响其他来源。
    fn query(&self, query: &MentionQuery) -> Result<Vec<MentionCandidate>, String>;
}

impl CoreManager {
    /// 宿主注册/更新候选来源。安装、卸载、启停后由宿主刷新。
    pub fn set_mention_sources(&self, sources: Vec<Arc<dyn MentionSource>>) {
        *self
            .mention_sources
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = sources;
    }

    /// 查询 @提及候选。
    ///
    /// 不创建 Core、不执行 turn、不调用模型；无效会话报错，不回退其他工作区。
    /// 调用方应在阻塞线程执行（来源可能同步调用 WASM/sidecar）。
    pub fn query_mentions(&self, request: MentionRequest) -> Result<Vec<MentionGroup>, String> {
        let context = match request.target {
            MentionTarget::Global => MentionContext::default(),
            MentionTarget::Draft { workspace } => MentionContext {
                session_id: None,
                workspace: Some(normalize_workspace(&workspace)?),
            },
            MentionTarget::Session { session_id } => {
                // 工作区取宿主权威会话状态；会话不存在即报错，不借用其他工作区。
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

        let sources = self
            .mention_sources
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let mut groups: BTreeMap<String, MentionGroup> = BTreeMap::new();
        let mut seen = HashSet::new();
        let needle = query.query.to_lowercase();
        for source in sources {
            // 单个来源失败只跳过它：mention 是输入辅助，不阻塞补全。
            let candidates = match source.query(&query) {
                Ok(candidates) => candidates,
                Err(error) => {
                    tracing::warn!(source = source.id(), %error, "mention 候选查询失败");
                    continue;
                }
            };
            for candidate in candidates {
                if !query.allowed_kinds.is_empty() && !query.allowed_kinds.contains(&candidate.kind)
                {
                    continue;
                }
                // 兜底匹配：来源未按 query 过滤时在此收敛（匹配先于截断）。
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
        return Err("工作区必须是绝对路径".to_string());
    }
    let path = path
        .canonicalize()
        .map_err(|error| format!("无法解析工作区：{error}"))?;
    if !path.is_dir() {
        return Err("工作区不是目录".to_string());
    }
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiangong_core::config::core::{CoreConfig, CoreConfigProvider};

    struct FakeSource;
    impl MentionSource for FakeSource {
        fn id(&self) -> &str {
            "fake"
        }
        fn query(&self, _query: &MentionQuery) -> Result<Vec<MentionCandidate>, String> {
            Ok((0..1500)
                .map(|index| MentionCandidate {
                    value: format!("@file:{index}"),
                    label: format!("file-{index}"),
                    kind: "file".to_string(),
                    ..Default::default()
                })
                .collect())
        }
    }

    fn manager(root: &Path) -> CoreManager {
        CoreManager::new(CoreConfigProvider::new(CoreConfig::default()), root)
    }

    #[test]
    fn 注入来源在无任何core时即可查询() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(root.path());
        manager.set_mention_sources(vec![Arc::new(FakeSource)]);

        // 第 1499 条超出前端旧上限：匹配发生在截断之前才可命中。
        let groups = manager
            .query_mentions(MentionRequest {
                query: "1499".to_string(),
                max_per_group: 1,
                ..Default::default()
            })
            .expect("查询应成功");
        assert_eq!(groups[0].candidates[0].value, "@file:1499");
        // 查询不创建 Core，也不落任何会话文件。
        assert!(!manager.has_live_core("any"));
        assert!(!root.path().join("sessions").exists());

        manager.set_mention_sources(Vec::new());
        assert!(
            manager
                .query_mentions(MentionRequest::default())
                .expect("查询应成功")
                .is_empty(),
            "来源移除后候选应立即为空"
        );
    }

    #[test]
    fn 无效会话不回退到其他工作区() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(root.path());
        manager.set_mention_sources(vec![Arc::new(FakeSource)]);
        for session_id in ["missing", "../escape"] {
            assert!(
                manager
                    .query_mentions(MentionRequest {
                        target: MentionTarget::Session {
                            session_id: session_id.to_string(),
                        },
                        ..Default::default()
                    })
                    .is_err(),
                "会话 {session_id} 必须报错而不是返回全局候选"
            );
        }
    }
}
