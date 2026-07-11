//! Desktop 统一工作区 Tab 的薄布局层。
//!
//! Browser/Terminal 插件各自持有并持久化 Tab 元数据；这里仅保存跨插件的
//! 混排引用和 UI 活跃引用，不复制 URL、标题、PTY 或 WebView 状态。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tiangong_core::session::atomic_replace_file;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceTabKind {
    Browser,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTabState {
    pub id: String,
    pub kind: WorkspaceTabKind,
    pub title: String,
    pub url: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceTabRef {
    pub kind: WorkspaceTabKind,
    pub id: String,
}

impl WorkspaceTabRef {
    pub fn matches(&self, tab: &WorkspaceTabState) -> bool {
        self.kind == tab.kind && self.id == tab.id
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTabsLayout {
    #[serde(default)]
    pub order: Vec<WorkspaceTabRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<WorkspaceTabRef>,
}

pub fn load_layout(session_id: &str) -> WorkspaceTabsLayout {
    load_layout_at(&storage_root(), session_id).unwrap_or_else(|error| {
        tracing::warn!(%error, session_id, "加载工作区标签页布局失败，按插件状态恢复");
        WorkspaceTabsLayout::default()
    })
}

fn load_layout_at(root: &Path, session_id: &str) -> Result<WorkspaceTabsLayout> {
    let path = layout_path(root, session_id);
    if !path.exists() {
        return Ok(WorkspaceTabsLayout::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("读取工作区标签页布局失败：{}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("解析工作区标签页布局失败：{}", path.display()))
}

pub fn save_layout(
    session_id: &str,
    tabs: &[WorkspaceTabState],
    active_tab_id: Option<&str>,
) -> Result<()> {
    let layout = layout_from_tabs(tabs, active_tab_id);
    save_layout_state_at(&storage_root(), session_id, &layout)
}

fn layout_from_tabs(
    tabs: &[WorkspaceTabState],
    active_tab_id: Option<&str>,
) -> WorkspaceTabsLayout {
    let mut order = Vec::with_capacity(tabs.len());
    for tab in tabs {
        let reference = WorkspaceTabRef {
            kind: tab.kind,
            id: tab.id.clone(),
        };
        if !order.contains(&reference) {
            order.push(reference);
        }
    }
    let active = active_tab_id.and_then(|active_id| {
        tabs.iter()
            .find(|tab| tab.id == active_id)
            .map(|tab| WorkspaceTabRef {
                kind: tab.kind,
                id: tab.id.clone(),
            })
    });
    WorkspaceTabsLayout { order, active }
}

pub fn reconcile_tabs(
    available: Vec<WorkspaceTabState>,
    layout: WorkspaceTabsLayout,
    fallback_active: &[WorkspaceTabRef],
) -> (Vec<WorkspaceTabState>, Option<String>) {
    let mut by_key = available
        .iter()
        .cloned()
        .map(|tab| ((tab.kind, tab.id.clone()), tab))
        .collect::<std::collections::HashMap<_, _>>();
    let mut tabs = Vec::with_capacity(available.len());
    for reference in &layout.order {
        if let Some(tab) = by_key.remove(&(reference.kind, reference.id.clone())) {
            tabs.push(tab);
        }
    }
    for tab in available {
        if let Some(tab) = by_key.remove(&(tab.kind, tab.id.clone())) {
            tabs.push(tab);
        }
    }

    let active = layout
        .active
        .filter(|reference| tabs.iter().any(|tab| reference.matches(tab)))
        .or_else(|| {
            fallback_active
                .iter()
                .find(|reference| tabs.iter().any(|tab| reference.matches(tab)))
                .cloned()
        })
        .map(|reference| reference.id)
        .or_else(|| tabs.first().map(|tab| tab.id.clone()));
    (tabs, active)
}

pub fn remove_layout(session_id: &str) -> Result<()> {
    let path = layout_path(&storage_root(), session_id);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("删除工作区标签页布局失败：{}", path.display()))?;
    }
    Ok(())
}

/// 必须在 `TiangongApp::new()` 之前调用，避免启动恢复先重写旧 Session JSON。
pub fn migrate_legacy_tabs() -> Result<()> {
    let root = storage_root();
    let mut values = Vec::new();
    let sessions_dir = root.join("sessions");
    if sessions_dir
        .try_exists()
        .with_context(|| format!("检查旧会话目录失败：{}", sessions_dir.display()))?
    {
        let mut paths = std::fs::read_dir(&sessions_dir)
            .with_context(|| format!("读取旧会话目录失败：{}", sessions_dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("读取旧会话失败：{}", path.display()))?;
            let value = serde_json::from_str::<Value>(&content)
                .with_context(|| format!("解析旧会话失败：{}", path.display()))?;
            values.push(value);
        }
    }
    let combined_path = root.join("sessions.json");
    if combined_path
        .try_exists()
        .with_context(|| format!("检查旧会话集合失败：{}", combined_path.display()))?
    {
        let content = std::fs::read_to_string(&combined_path)
            .with_context(|| format!("读取旧会话集合失败：{}", combined_path.display()))?;
        let value = serde_json::from_str::<Value>(&content)
            .with_context(|| format!("解析旧会话集合失败：{}", combined_path.display()))?;
        if let Some(sessions) = value.get("sessions").and_then(Value::as_array) {
            values.extend(sessions.iter().cloned());
        }
    }

    for value in values {
        let Some(session_id) = value.get("id").and_then(Value::as_str) else {
            continue;
        };
        tiangong_plugin_browser::session_store::BrowserSessionStore::migrate_legacy_value(
            session_id, &value,
        )?;
        tiangong_plugin_terminal::session_store::TerminalSessionStore::migrate_legacy_value(
            session_id, &value,
        )?;
        migrate_legacy_layout_at(&root, session_id, &value)?;
    }
    Ok(())
}

fn migrate_legacy_layout_at(root: &Path, session_id: &str, value: &Value) -> Result<()> {
    let path = layout_path(root, session_id);
    if path.exists() {
        return Ok(());
    }
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    if !object.contains_key("tabs") && !object.contains_key("active_tab_id") {
        return Ok(());
    }
    let order = object
        .get("tabs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tab| {
            let kind = match tab.get("kind").and_then(Value::as_str)? {
                "browser" => WorkspaceTabKind::Browser,
                "terminal" => WorkspaceTabKind::Terminal,
                _ => return None,
            };
            let id = tab.get("id")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            Some(WorkspaceTabRef {
                kind,
                id: id.to_string(),
            })
        })
        .fold(Vec::new(), |mut refs, reference| {
            if !refs.contains(&reference) {
                refs.push(reference);
            }
            refs
        });
    let active = object
        .get("active_tab_id")
        .and_then(Value::as_str)
        .and_then(|active| order.iter().find(|tab| tab.id == active).cloned());
    save_layout_state_at(root, session_id, &WorkspaceTabsLayout { order, active })
}

fn save_layout_state_at(root: &Path, session_id: &str, layout: &WorkspaceTabsLayout) -> Result<()> {
    let path = layout_path(root, session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建工作区标签页布局目录失败：{}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(layout)
        .with_context(|| format!("序列化工作区标签页布局失败：{session_id}"))?;
    atomic_replace_file(&path, content.as_bytes())
        .with_context(|| format!("写入工作区标签页布局失败：{}", path.display()))
}

fn storage_root() -> PathBuf {
    tiangong_config::io::storage_root()
}

fn layout_path(root: &Path, session_id: &str) -> PathBuf {
    root.join("workspace-tab-layouts")
        .join(format!("{}.json", sanitize(session_id)))
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_contains_only_refs() {
        let tabs = vec![
            WorkspaceTabState {
                id: "terminal-1".to_string(),
                kind: WorkspaceTabKind::Terminal,
                title: "不会写入布局".to_string(),
                url: "/secret".to_string(),
                created_at: "now".to_string(),
            },
            WorkspaceTabState {
                id: "terminal-1".to_string(),
                kind: WorkspaceTabKind::Terminal,
                title: "重复项".to_string(),
                url: String::new(),
                created_at: "later".to_string(),
            },
        ];
        let layout = layout_from_tabs(&tabs, Some("terminal-1"));
        let json = serde_json::to_string(&layout).unwrap();
        assert!(!json.contains("不会写入布局"));
        assert!(!json.contains("/secret"));
        assert!(json.contains("terminal-1"));
        assert_eq!(layout.order.len(), 1);
        assert_eq!(layout.active, layout.order.first().cloned());
    }

    #[test]
    fn reconcile_preserves_mixed_order_and_prunes_stale_refs() {
        let terminal = WorkspaceTabState {
            id: "terminal-1".to_string(),
            kind: WorkspaceTabKind::Terminal,
            title: "终端".to_string(),
            url: String::new(),
            created_at: "1".to_string(),
        };
        let browser = WorkspaceTabState {
            id: "browser-1".to_string(),
            kind: WorkspaceTabKind::Browser,
            title: "网页".to_string(),
            url: "https://example.com".to_string(),
            created_at: String::new(),
        };
        let added = WorkspaceTabState {
            id: "browser-2".to_string(),
            kind: WorkspaceTabKind::Browser,
            title: "新增".to_string(),
            url: "about:blank".to_string(),
            created_at: String::new(),
        };
        let layout = WorkspaceTabsLayout {
            order: vec![
                WorkspaceTabRef {
                    kind: WorkspaceTabKind::Browser,
                    id: "stale".to_string(),
                },
                WorkspaceTabRef {
                    kind: WorkspaceTabKind::Browser,
                    id: browser.id.clone(),
                },
                WorkspaceTabRef {
                    kind: WorkspaceTabKind::Terminal,
                    id: terminal.id.clone(),
                },
            ],
            active: Some(WorkspaceTabRef {
                kind: WorkspaceTabKind::Browser,
                id: "stale".to_string(),
            }),
        };
        let fallback = [WorkspaceTabRef {
            kind: WorkspaceTabKind::Terminal,
            id: terminal.id.clone(),
        }];

        let (tabs, active) = reconcile_tabs(vec![terminal, added, browser], layout, &fallback);
        assert_eq!(
            tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(),
            vec!["browser-1", "terminal-1", "browser-2"]
        );
        assert_eq!(active.as_deref(), Some("terminal-1"));
    }

    #[test]
    fn legacy_layout_migrates_once_as_refs_only() -> Result<()> {
        let root = tempfile::tempdir()?;
        let legacy = serde_json::json!({
            "id": "session-1",
            "tabs": [
                {"id":"browser-1","kind":"browser","title":"网页","url":"https://example.com"},
                {"id":"terminal-1","kind":"terminal","title":"终端","created_at":"now"}
            ],
            "active_tab_id": "terminal-1"
        });
        migrate_legacy_layout_at(root.path(), "session-1", &legacy)?;
        let layout = load_layout_at(root.path(), "session-1")?;
        assert_eq!(layout.order.len(), 2);
        assert_eq!(
            layout.active.as_ref().map(|tab| tab.id.as_str()),
            Some("terminal-1")
        );
        let json = serde_json::to_string(&layout)?;
        assert!(!json.contains("https://example.com"));
        assert!(!json.contains("title"));

        save_layout_state_at(root.path(), "session-1", &WorkspaceTabsLayout::default())?;
        migrate_legacy_layout_at(root.path(), "session-1", &legacy)?;
        assert_eq!(
            load_layout_at(root.path(), "session-1")?,
            WorkspaceTabsLayout::default()
        );
        Ok(())
    }
}
