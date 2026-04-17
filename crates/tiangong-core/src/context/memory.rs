//! 用户偏好与长期记忆管理（已废弃）
//!
//! 此模块已被 `tiangong-memory` crate 替代。
//! 注入上下文现在通过 `tiangong_memory::load_injection_sync` 读取三级 Injection 文件。
//!
//! 保留此文件仅供过渡期兼容，后续版本将移除。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// 记忆唯一键
    pub key: String,
    /// 记忆内容
    pub content: String,
    /// 记忆范围
    pub scope: MemoryScope,
    /// 创建时间
    pub created_at: String,
    /// 最后更新时间
    pub updated_at: String,
}

/// 记忆范围
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// 全局记忆（所有会话共享）
    Global,
    /// 会话级记忆（仅特定会话可见）
    Session { session_id: String },
}

/// 记忆存储
///
/// 管理用户偏好和长期记忆的加载与查询。
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: HashMap<String, MemoryEntry>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从磁盘加载记忆
    pub fn load_from_disk() -> Self {
        let dir = memory_dir();
        if !dir.exists() {
            return Self::default();
        }

        let mut store = Self::new();

        // 加载全局记忆
        let global_path = dir.join("global.json");
        if let Ok(content) = fs::read_to_string(&global_path)
            && let Ok(entries) = serde_json::from_str::<Vec<MemoryEntry>>(&content)
        {
            for entry in entries {
                store.entries.insert(entry.key.clone(), entry);
            }
        }

        store
    }

    /// 获取某会话可见的记忆（全局 + 会话级）
    pub fn get_for_session(&self, session_id: &str) -> Vec<&MemoryEntry> {
        self.entries
            .values()
            .filter(|e| match &e.scope {
                MemoryScope::Global => true,
                MemoryScope::Session { session_id: sid } => sid == session_id,
            })
            .collect()
    }

    /// 获取所有全局记忆
    pub fn get_global(&self) -> Vec<&MemoryEntry> {
        self.entries
            .values()
            .filter(|e| e.scope == MemoryScope::Global)
            .collect()
    }

    /// 添加或更新记忆
    pub fn upsert(&mut self, entry: MemoryEntry) {
        self.entries.insert(entry.key.clone(), entry);
    }

    /// 删除记忆
    pub fn remove(&mut self, key: &str) -> Option<MemoryEntry> {
        self.entries.remove(key)
    }

    /// 将记忆格式化为上下文注入文本
    pub fn format_for_context(&self, session_id: &str) -> Option<String> {
        let memories = self.get_for_session(session_id);
        if memories.is_empty() {
            return None;
        }

        let mut lines = vec!["[用户偏好与记忆]".to_string()];
        for entry in memories {
            lines.push(format!("- {}: {}", entry.key, entry.content));
        }
        Some(lines.join("\n"))
    }

    /// 记忆条目数量
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 获取记忆存储目录
fn memory_dir() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("memory")
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(key: &str, content: &str, scope: MemoryScope) -> MemoryEntry {
        let now = chrono::Local::now().naive_local().to_string();
        MemoryEntry {
            key: key.into(),
            content: content.into(),
            scope,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    #[test]
    fn upsert_and_get() {
        let mut store = MemoryStore::new();
        store.upsert(make_entry("lang", "偏好中文回复", MemoryScope::Global));
        assert_eq!(store.len(), 1);
        assert_eq!(store.get_global().len(), 1);
    }

    #[test]
    fn session_scoped_memory() {
        let mut store = MemoryStore::new();
        store.upsert(make_entry("global_pref", "全局偏好", MemoryScope::Global));
        store.upsert(make_entry(
            "session_pref",
            "会话偏好",
            MemoryScope::Session {
                session_id: "s1".into(),
            },
        ));

        // s1 能看到两条
        assert_eq!(store.get_for_session("s1").len(), 2);
        // s2 只能看到全局的
        assert_eq!(store.get_for_session("s2").len(), 1);
    }

    #[test]
    fn format_for_context() {
        let mut store = MemoryStore::new();
        store.upsert(make_entry("lang", "偏好中文", MemoryScope::Global));
        let text = store.format_for_context("any").unwrap();
        assert!(text.contains("[用户偏好与记忆]"));
        assert!(text.contains("偏好中文"));
    }

    #[test]
    fn empty_store_no_context() {
        let store = MemoryStore::new();
        assert!(store.format_for_context("any").is_none());
    }

    #[test]
    fn remove_entry() {
        let mut store = MemoryStore::new();
        store.upsert(make_entry("key1", "val", MemoryScope::Global));
        assert_eq!(store.len(), 1);
        store.remove("key1");
        assert!(store.is_empty());
    }

    #[test]
    fn serialization_roundtrip() {
        let entry = make_entry(
            "test",
            "content",
            MemoryScope::Session {
                session_id: "s1".into(),
            },
        );
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MemoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.key, "test");
        assert!(matches!(parsed.scope, MemoryScope::Session { .. }));
    }
}
