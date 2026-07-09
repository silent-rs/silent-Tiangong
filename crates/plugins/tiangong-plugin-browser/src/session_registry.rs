//! 浏览器 per-session 状态注册表。
//!
//! 每个 Core（session）拥有独立的 [`BrowserState`]（webview / tab / 历史 / 轮询标志），
//! 多个 session 的 webview 可并发存活，切换 session 时只切换可见性，不销毁 webview。
//!
//! 设计镜像 terminal 插件的 `SessionPtyRegistry`：`sessions` HashMap 懒创建，
//! `active_session_id` 跟踪当前可见的 session。`BrowserManager` 的方法通过
//! [`active_state`](BrowserSessionRegistry::active_state) 获取当前 session 的 state，
//! 前端命令作用于 active session（符合现状）；agent 工具调用经 session_id 显式路由。
//!
//! 详见 RFC 0016。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::manager::BrowserState;

/// 浏览器 per-session 状态注册表。
///
/// 持有所有 session 的 `BrowserState`，以及当前 active session id。
/// 由 `BrowserPluginState` 单例持有，`BrowserManager` / handler / commands 经它路由。
///
/// `global_history` 与 `zoom_factor` 是进程级共享数据（跨 session），存于 registry
/// 而非每个 session 的 `BrowserState`，避免 N 份副本割裂。
pub struct BrowserSessionRegistry {
    /// session_id -> 该 session 的浏览器状态（懒创建）
    sessions: Mutex<HashMap<String, Arc<Mutex<BrowserState>>>>,
    /// 当前可见的 session id（前端正在查看的对话）
    active_session_id: Mutex<Option<String>>,
    /// 全局浏览历史（跨 session 共享，持久化在 ~/.tiangong/browser-history.json）
    global_history: Mutex<Vec<crate::types::HistoryEntry>>,
    /// 当前页面缩放比例（进程级用户偏好，持久化在 ~/.tiangong/browser-zoom.json）
    zoom_factor: std::sync::atomic::AtomicU64,
}

impl BrowserSessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            active_session_id: Mutex::new(None),
            global_history: Mutex::new(crate::manager::load_global_history()),
            zoom_factor: std::sync::atomic::AtomicU64::new(crate::manager::load_zoom().to_bits()),
        }
    }

    /// 获取或懒创建指定 session 的 state。
    ///
    /// 首次访问时创建一个空的 `BrowserState`。
    pub fn session_state(&self, session_id: &str) -> Arc<Mutex<BrowserState>> {
        let mut sessions = self.sessions.lock().expect("browser sessions poisoned");
        sessions
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(BrowserState::new_empty())))
            .clone()
    }

    /// 获取指定 session 的已有 state（不存在返回 None，不创建）。
    pub fn existing_session_state(&self, session_id: &str) -> Option<Arc<Mutex<BrowserState>>> {
        self.sessions
            .lock()
            .expect("browser sessions poisoned")
            .get(session_id)
            .cloned()
    }

    /// 获取当前 active session 的 state。
    ///
    /// 无 active session 时，返回首个已注册 session 的 state（兼容早期单 session 启动
    /// 场景）；完全无 session 时返回 None。`BrowserManager` 的大多数方法经此获取 state，
    /// 前端命令作用于 active session。
    pub fn active_state(&self) -> Option<Arc<Mutex<BrowserState>>> {
        let sessions = self.sessions.lock().expect("browser sessions poisoned");
        let active = self
            .active_session_id
            .lock()
            .expect("active_session_id poisoned");
        if let Some(id) = active.as_ref() {
            if let Some(state) = sessions.get(id) {
                return Some(state.clone());
            }
        }
        // 无 active session：回退到首个已注册的（兼容旧的单 session 启动路径）
        sessions.values().next().cloned()
    }

    /// 设置当前 active session（前端切换对话时调用）。
    pub fn set_active(&self, session_id: &str) {
        // 确保 session 已注册
        self.session_state(session_id);
        let mut active = self
            .active_session_id
            .lock()
            .expect("active_session_id poisoned");
        *active = Some(session_id.to_string());
    }

    /// 当前 active session id。
    pub fn active_session_id(&self) -> Option<String> {
        self.active_session_id
            .lock()
            .expect("active_session_id poisoned")
            .clone()
    }

    /// 销毁指定 session 的全部状态（关闭 webview、清理）。
    ///
    /// session 被删除时调用。webview 的实际关闭由调用方在 drop 前显式处理。
    pub fn destroy_session(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().expect("browser sessions poisoned");
        sessions.remove(session_id);
        let mut active = self
            .active_session_id
            .lock()
            .expect("active_session_id poisoned");
        if active.as_deref() == Some(session_id) {
            *active = sessions.keys().next().cloned();
        }
    }

    /// 全局浏览历史快照（跨 session 共享）。
    pub fn global_history(&self) -> Vec<crate::types::HistoryEntry> {
        self.global_history
            .lock()
            .expect("global_history poisoned")
            .clone()
    }

    /// 当前缩放比例（进程级用户偏好）。
    pub fn zoom_factor(&self) -> f64 {
        f64::from_bits(self.zoom_factor.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// 设置缩放比例（进程级）。
    pub fn set_zoom_factor(&self, zoom: f64) {
        self.zoom_factor
            .store(zoom.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
}

impl Default for BrowserSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
