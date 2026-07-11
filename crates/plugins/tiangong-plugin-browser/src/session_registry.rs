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

use tracing::warn;

use crate::manager::{BrowserSharedState, BrowserState};

/// 浏览器 per-session 状态注册表。
///
/// 持有所有 session 的 `BrowserState`，以及当前 active session id。
/// 由 `BrowserPluginState` 单例持有，`BrowserManager` / handler / commands 经它路由。
///
/// global_history / zoom 由 `shared` 在进程内共享并统一持久化。
pub struct BrowserSessionRegistry {
    /// session_id -> 该 session 的浏览器状态（懒创建）
    sessions: Mutex<HashMap<String, Arc<Mutex<BrowserState>>>>,
    /// 当前可见的 session id（前端正在查看的对话）
    active_session_id: Mutex<Option<String>>,
    /// 所有 session 共用的全局浏览历史和缩放设置。
    shared: Arc<BrowserSharedState>,
}

impl BrowserSessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            active_session_id: Mutex::new(None),
            shared: Arc::new(BrowserSharedState::load()),
        }
    }

    /// 获取或懒创建指定 session 的 state。
    ///
    /// 首次访问时创建一个空的 `BrowserState`。
    pub fn session_state(&self, session_id: &str) -> Arc<Mutex<BrowserState>> {
        let mut sessions = self.sessions.lock().expect("browser sessions poisoned");
        sessions
            .entry(session_id.to_string())
            .or_insert_with(|| {
                Arc::new(Mutex::new(BrowserState::new_empty(
                    session_id.to_string(),
                    self.shared.clone(),
                )))
            })
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

    /// 获取任意一个已注册 session 的 state（不创建）。
    /// 供进程级操作（global history）使用——不依赖 active session，不创建 bootstrap。
    pub fn sessions_count_checked(&self) -> Option<Arc<Mutex<BrowserState>>> {
        self.sessions
            .lock()
            .expect("browser sessions poisoned")
            .values()
            .next()
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
    /// 草稿 session 转正：关闭 draft WebViews，迁移 tabs 到 persistent store，
    /// 删除 draft state。persistent session 后续 hydrate 时从 store 重建 WebViews。
    ///
    /// 不直接迁移存活 WebView（其 on_page_load closure 捕获了 draft session_id，
    /// data directory / label 也按 draft id 创建）。转正后切换到该 session 时
    /// 由 switch_session 从 store 重新创建。
    pub fn attach_session(&self, draft_id: &str, persistent_id: &str) {
        if draft_id == persistent_id || draft_id.is_empty() || persistent_id.is_empty() {
            return;
        }
        let removed = {
            let mut sessions = self.sessions.lock().expect("browser sessions poisoned");
            sessions.remove(draft_id)
        };
        let runtime_state = if let Some(state_arc) = removed {
            let mut s = state_arc.lock().unwrap_or_else(|e| e.into_inner());
            // 停 draft 轮询
            s.poll_stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
            s.event_poll_stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
            // 关闭 draft WebViews
            for (_, wv) in s.webviews.drain() {
                let _ = wv.close();
            }
            // 保存全部 tabs；about:blank 只延迟创建 WebView，不代表无需恢复。
            let tabs = s.tabs.clone();
            let active_tab_id = s
                .active_tab_id
                .as_ref()
                .filter(|id| tabs.iter().any(|t| &t.id == *id))
                .cloned();
            Some(crate::session_store::BrowserSessionPersisted {
                tabs,
                active_tab_id,
            })
        } else {
            None
        };

        let mut draft_state = match crate::session_store::BrowserSessionStore::load(draft_id) {
            Ok(state) => state,
            Err(error) => {
                warn!(%error, draft_id, "加载草稿浏览器会话状态失败，保留原状态");
                return;
            }
        };
        if let Some(runtime_state) = runtime_state {
            draft_state = runtime_state;
        }
        let mut target_state = match crate::session_store::BrowserSessionStore::load(persistent_id)
        {
            Ok(state) => state,
            Err(error) => {
                warn!(%error, persistent_id, "加载目标浏览器会话状态失败，保留草稿状态");
                return;
            }
        };
        let mut target_ids = target_state
            .tabs
            .iter()
            .map(|tab| tab.id.clone())
            .collect::<std::collections::HashSet<_>>();
        for tab in draft_state.tabs {
            if target_ids.insert(tab.id.clone()) {
                target_state.tabs.push(tab);
            }
        }
        if target_state.active_tab_id.is_none() {
            target_state.active_tab_id = draft_state.active_tab_id;
        }
        if let Err(error) =
            crate::session_store::BrowserSessionStore::save(persistent_id, &target_state)
        {
            warn!(%error, persistent_id, "持久化转正浏览器会话状态失败");
            return;
        }
        if let Err(error) = crate::session_store::BrowserSessionStore::remove(draft_id) {
            warn!(%error, draft_id, "删除草稿浏览器会话状态失败");
        }
        // 更新 active
        let mut active = self
            .active_session_id
            .lock()
            .expect("active_session_id poisoned");
        if active.as_deref() == Some(draft_id) {
            *active = Some(persistent_id.to_string());
        }
    }

    /// 销毁指定 session：停轮询、关闭 webview、清理 state、删持久化文件。
    pub fn destroy_session(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().expect("browser sessions poisoned");
        if let Some(state_arc) = sessions.remove(session_id) {
            // 停轮询 + 关闭 webview + 清运行时 state
            let mut s = state_arc.lock().unwrap_or_else(|e| e.into_inner());
            s.poll_stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
            s.event_poll_stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
            for (_, wv) in s.webviews.drain() {
                let _ = wv.close();
            }
            s.tabs.clear();
            s.active_tab_id = None;
        }
        let mut active = self
            .active_session_id
            .lock()
            .expect("active_session_id poisoned");
        if active.as_deref() == Some(session_id) {
            *active = sessions.keys().next().cloned();
        }
        // 删除持久化文件
        if let Err(error) = crate::session_store::BrowserSessionStore::remove(session_id) {
            warn!(%error, session_id, "删除浏览器会话状态失败");
        }
    }
}

impl Default for BrowserSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
