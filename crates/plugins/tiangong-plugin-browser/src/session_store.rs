//! 浏览器 per-session 持久化存储。
//!
//! 每个 session 的浏览器恢复数据（tab/url/title/active_tab_id）持久化到
//! `~/.tiangong/browser-sessions/<session_id>.json`，应用重启后按 session 恢复。
//!
//! 详见 RFC 0016。Core `Session.tabs` 的 browser tab 字段保留兼容（前端仍可读写），
//! 本 store 作为浏览器插件自管的补充真相源。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::BrowserTab;

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
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".tiangong")
            .join("browser-sessions")
            .join(format!("{}.json", sanitize(session_id)))
    }

    /// 加载指定 session 的持久化状态（不存在返回空）。
    pub fn load(session_id: &str) -> BrowserSessionPersisted {
        let path = Self::path(session_id);
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => BrowserSessionPersisted::default(),
        }
    }

    /// 保存指定 session 的状态。
    pub fn save(session_id: &str, state: &BrowserSessionPersisted) {
        let path = Self::path(session_id);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(content) = serde_json::to_string_pretty(state) {
            let _ = std::fs::write(path, content);
        }
    }

    /// 删除指定 session 的持久化文件（session 销毁时调用）。
    pub fn remove(session_id: &str) {
        let path = Self::path(session_id);
        let _ = std::fs::remove_file(path);
    }
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
