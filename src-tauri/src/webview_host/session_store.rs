//! 浏览器 per-session 持久化存储。
//!
//! 每个 session 的浏览器恢复数据（tab/url/title/active_tab_id）持久化到
//! `~/.tiangong/browser-sessions/<session_id>.json`，应用重启后按 session 恢复。
//!
//! 详见 RFC 0016。BrowserSessionStore 是浏览器 Tab 元数据的唯一真相源；
//! Core Session 不持有浏览器或工作区 Tab 状态。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tiangong_core::session::atomic_replace_file;

use crate::webview_host::types::{BrowserTab, BrowserTabSource};

/// 单个 session 的持久化浏览器状态。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BrowserSessionPersisted {
    pub tabs: Vec<BrowserTab>,
    pub active_tab_id: Option<String>,
}

/// per-session 存储的存取（文件级，无运行时状态）。
pub struct BrowserSessionStore;

impl BrowserSessionStore {
    /// session 持久化文件路径。
    fn path(session_id: &str) -> PathBuf {
        state_path(&tiangong_config::io::storage_root(), session_id)
    }

    /// 加载指定 session 的持久化状态（不存在返回空）。
    pub fn load(session_id: &str) -> Result<BrowserSessionPersisted> {
        Self::load_at(&tiangong_config::io::storage_root(), session_id)
    }

    /// 从旧 Core Session 迁移 browser tabs。已有插件文件（包括显式空状态）
    /// 始终优先，避免用户关闭全部标签页后旧数据复活。
    pub fn migrate_legacy_value(session_id: &str, value: &Value) -> Result<()> {
        let root = tiangong_config::io::storage_root();
        Self::migrate_legacy_value_at(&root, session_id, value)
    }

    fn migrate_legacy_value_at(root: &Path, session_id: &str, value: &Value) -> Result<()> {
        if state_path(root, session_id)
            .try_exists()
            .with_context(|| format!("检查浏览器会话状态失败：{session_id}"))?
        {
            return Ok(());
        }
        let Some(object) = value.as_object() else {
            return Ok(());
        };
        if !object.contains_key("tabs") && !object.contains_key("active_tab_id") {
            return Ok(());
        }
        let tabs = object
            .get("tabs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|tab| tab.get("kind").and_then(Value::as_str) == Some("browser"))
            .filter_map(|tab| {
                let id = tab.get("id")?.as_str()?.trim();
                if id.is_empty() {
                    return None;
                }
                Some(BrowserTab {
                    id: id.to_string(),
                    url: tab
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("about:blank")
                        .to_string(),
                    title: tab
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("新标签页")
                        .to_string(),
                    source: BrowserTabSource::User,
                    agent_domain: None,
                })
            })
            .collect::<Vec<_>>();
        let active_tab_id = object
            .get("active_tab_id")
            .and_then(Value::as_str)
            .filter(|active| tabs.iter().any(|tab| tab.id == *active))
            .map(ToString::to_string);
        Self::save_at(
            root,
            session_id,
            &BrowserSessionPersisted {
                tabs,
                active_tab_id,
            },
        )
    }

    /// 保存指定 session 的状态。
    pub fn save(session_id: &str, state: &BrowserSessionPersisted) -> Result<()> {
        Self::save_at(&tiangong_config::io::storage_root(), session_id, state)
    }

    /// 删除指定 session 的持久化文件（session 销毁时调用）。
    pub fn remove(session_id: &str) -> Result<()> {
        let path = Self::path(session_id);
        if path
            .try_exists()
            .with_context(|| format!("检查浏览器会话状态失败：{}", path.display()))?
        {
            std::fs::remove_file(&path)
                .with_context(|| format!("删除浏览器会话状态失败：{}", path.display()))?;
        }
        Ok(())
    }

    fn load_at(root: &Path, session_id: &str) -> Result<BrowserSessionPersisted> {
        let path = state_path(root, session_id);
        if !path
            .try_exists()
            .with_context(|| format!("检查浏览器会话状态失败：{}", path.display()))?
        {
            return Ok(BrowserSessionPersisted::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("读取浏览器会话状态失败：{}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("解析浏览器会话状态失败：{}", path.display()))
    }

    fn save_at(root: &Path, session_id: &str, state: &BrowserSessionPersisted) -> Result<()> {
        let path = state_path(root, session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建浏览器会话状态目录失败：{}", parent.display()))?;
        }
        let content = serde_json::to_string_pretty(state)
            .with_context(|| format!("序列化浏览器会话状态失败：{session_id}"))?;
        atomic_replace_file(&path, content.as_bytes())
            .with_context(|| format!("写入浏览器会话状态失败：{}", path.display()))
    }
}

fn state_path(root: &Path, session_id: &str) -> PathBuf {
    root.join("browser-sessions")
        .join(format!("{}.json", sanitize(session_id)))
}

/// 规范化 session id 用于文件名（避免路径穿越）。
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
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
    fn legacy_migration_keeps_blank_tabs_and_explicit_empty_store_wins() -> Result<()> {
        let root = tempfile::tempdir()?;
        let session_id = "session-1";
        let legacy = serde_json::json!({
            "id": session_id,
            "tabs": [
                {"id":"browser-1","kind":"browser","title":"新标签页","url":"about:blank","created_at":""},
                {"id":"terminal-1","kind":"terminal","title":"终端","url":"","created_at":"now"}
            ],
            "active_tab_id": "browser-1"
        });

        BrowserSessionStore::migrate_legacy_value_at(root.path(), session_id, &legacy)?;
        let migrated = BrowserSessionStore::load_at(root.path(), session_id)?;
        assert_eq!(migrated.tabs.len(), 1);
        assert_eq!(migrated.tabs[0].url, "about:blank");
        assert_eq!(migrated.tabs[0].source, BrowserTabSource::User);
        assert!(migrated.tabs[0].agent_domain.is_none());

        BrowserSessionStore::save_at(root.path(), session_id, &BrowserSessionPersisted::default())?;
        BrowserSessionStore::migrate_legacy_value_at(root.path(), session_id, &legacy)?;
        let restored = BrowserSessionStore::load_at(root.path(), session_id)?;
        assert!(restored.tabs.is_empty());
        assert!(restored.active_tab_id.is_none());
        Ok(())
    }
}
