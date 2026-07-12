//! 终端插件按会话持久化的 Tab 恢复元数据。
//!
//! 只保存终端插件拥有的 `id/title/created_at/active_tab_id`；PTY、cwd、shell、
//! alive 和协作状态均为运行态，不进入磁盘。旧版 Core Session 中的 terminal
//! tabs 仅作为一次性迁移输入。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tiangong_core::session::atomic_replace_file;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalTabPersisted {
    pub id: String,
    pub title: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSessionPersisted {
    #[serde(default)]
    pub tabs: Vec<TerminalTabPersisted>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tab_id: Option<String>,
}

pub struct TerminalSessionStore;

impl TerminalSessionStore {
    pub fn load(session_id: &str) -> Result<TerminalSessionPersisted> {
        Self::load_at(&tiangong_config::io::storage_root(), session_id)
    }

    /// 加载插件自有状态；文件尚不存在时，从旧 Core Session 做一次性迁移。
    /// 插件文件本身是迁移哨兵，显式空文件也不会被旧 Session 状态覆盖。
    pub fn load_or_migrate_legacy(session_id: &str) -> Result<TerminalSessionPersisted> {
        Self::load_or_migrate_legacy_at(&tiangong_config::io::storage_root(), session_id)
    }

    pub fn save(session_id: &str, state: &TerminalSessionPersisted) -> Result<()> {
        Self::save_at(&tiangong_config::io::storage_root(), session_id, state)
    }

    /// 从启动阶段已经读取的旧 Session JSON 迁移 terminal tabs。
    pub fn migrate_legacy_value(session_id: &str, value: &Value) -> Result<()> {
        let root = tiangong_config::io::storage_root();
        let path = state_path(&root, session_id);
        if path
            .try_exists()
            .with_context(|| format!("检查终端会话状态是否存在失败：{}", path.display()))?
        {
            return Ok(());
        }
        if let Some(state) = terminal_state_from_legacy(value) {
            Self::save_at(&root, session_id, &state)?;
        }
        Ok(())
    }

    pub fn remove(session_id: &str) -> Result<()> {
        let path = state_path(&tiangong_config::io::storage_root(), session_id);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("删除终端会话状态失败：{}", path.display()))?;
        }
        Ok(())
    }

    fn load_at(root: &Path, session_id: &str) -> Result<TerminalSessionPersisted> {
        let path = state_path(root, session_id);
        if !path.exists() {
            return Ok(TerminalSessionPersisted::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("读取终端会话状态失败：{}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("解析终端会话状态失败：{}", path.display()))
    }

    fn load_or_migrate_legacy_at(
        root: &Path,
        session_id: &str,
    ) -> Result<TerminalSessionPersisted> {
        let path = state_path(root, session_id);
        if path
            .try_exists()
            .with_context(|| format!("检查终端会话状态是否存在失败：{}", path.display()))?
        {
            return Self::load_at(root, session_id);
        }

        let Some(legacy_value) = legacy_session_value(root, session_id) else {
            return Ok(TerminalSessionPersisted::default());
        };
        let Some(state) = terminal_state_from_legacy(&legacy_value) else {
            return Ok(TerminalSessionPersisted::default());
        };
        Self::save_at(root, session_id, &state)?;
        Ok(state)
    }

    fn save_at(root: &Path, session_id: &str, state: &TerminalSessionPersisted) -> Result<()> {
        let path = state_path(root, session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建终端会话状态目录失败：{}", parent.display()))?;
        }
        let content = serde_json::to_string_pretty(state)
            .with_context(|| format!("序列化终端会话状态失败：{session_id}"))?;
        atomic_replace_file(&path, content.as_bytes())
            .with_context(|| format!("写入终端会话状态失败：{}", path.display()))
    }
}

fn state_path(root: &Path, session_id: &str) -> PathBuf {
    root.join("terminal-sessions")
        .join(format!("{}.json", sanitize(session_id)))
}

fn legacy_session_value(root: &Path, session_id: &str) -> Option<Value> {
    let split_path = root
        .join("sessions")
        .join(format!("{}.json", sanitize(session_id)));
    if let Ok(content) = std::fs::read_to_string(split_path) {
        if let Ok(value) = serde_json::from_str::<Value>(&content) {
            return Some(value);
        }
    }

    let content = std::fs::read_to_string(root.join("sessions.json")).ok()?;
    let value = serde_json::from_str::<Value>(&content).ok()?;
    value
        .get("sessions")?
        .as_array()?
        .iter()
        .find(|session| session.get("id").and_then(Value::as_str) == Some(session_id))
        .cloned()
}

fn terminal_state_from_legacy(value: &Value) -> Option<TerminalSessionPersisted> {
    let object = value.as_object()?;
    if !object.contains_key("tabs") && !object.contains_key("active_tab_id") {
        return None;
    }

    let tabs = object
        .get("tabs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|tab| tab.get("kind").and_then(Value::as_str) == Some("terminal"))
        .filter_map(|tab| {
            let id = tab.get("id")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            Some(TerminalTabPersisted {
                id: id.to_string(),
                title: tab
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("终端")
                    .to_string(),
                created_at: tab
                    .get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect::<Vec<_>>();
    let active_tab_id = object
        .get("active_tab_id")
        .and_then(Value::as_str)
        .filter(|active| tabs.iter().any(|tab| tab.id == *active))
        .map(ToString::to_string);
    Some(TerminalSessionPersisted {
        tabs,
        active_tab_id,
    })
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
    fn legacy_terminal_tabs_migrate_once_and_empty_store_wins() -> Result<()> {
        let root = tempfile::tempdir()?;
        let session_id = "session-1";
        let sessions_dir = root.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        std::fs::write(
            sessions_dir.join("session-1.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": session_id,
                "tabs": [
                    {"id":"browser-1","kind":"browser","title":"网页","url":"https://example.com","created_at":""},
                    {"id":"terminal-1","kind":"terminal","title":"终端 A","url":"","created_at":"2026-07-12"}
                ],
                "active_tab_id": "terminal-1"
            }))?,
        )?;

        let migrated = TerminalSessionStore::load_or_migrate_legacy_at(root.path(), session_id)?;
        assert_eq!(migrated.tabs.len(), 1);
        assert_eq!(migrated.tabs[0].id, "terminal-1");
        assert_eq!(migrated.active_tab_id.as_deref(), Some("terminal-1"));

        TerminalSessionStore::save_at(
            root.path(),
            session_id,
            &TerminalSessionPersisted::default(),
        )?;
        let restored = TerminalSessionStore::load_or_migrate_legacy_at(root.path(), session_id)?;
        assert_eq!(restored, TerminalSessionPersisted::default());
        Ok(())
    }
}
