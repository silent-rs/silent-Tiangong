//! 浏览器 per-session 状态注册表。
//!
//! 每个 Core（session）拥有独立的 [`BrowserState`]（webview / tab / 历史 / 轮询标志），
//! 多个 session 的 webview 可并发存活，切换 session 时只切换可见性，不销毁 webview。
//! 状态仅驻留当前应用进程，不从磁盘恢复标签。
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

use crate::webview_host::manager::{BrowserSharedState, BrowserState};

/// 浏览器 per-session 状态注册表。
///
/// 持有所有 session 的 `BrowserState`，以及当前 active session id。
/// 由 `WebviewHostState` 单例持有，`BrowserManager` / handler / commands 经它路由。
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

    /// 获取或懒创建指定 session 的进程内 state。
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

    /// 物理删除会话时销毁其全部 WebView 作用域并清理历史持久化文件。
    pub fn destroy_session(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().expect("browser sessions poisoned");
        let scopes = sessions
            .keys()
            .filter(|scope| scope_belongs_to_session(scope, session_id))
            .cloned()
            .collect::<Vec<_>>();
        for scope in scopes {
            let Some(state_arc) = sessions.remove(&scope) else {
                continue;
            };
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
        if active
            .as_deref()
            .is_some_and(|scope| scope_belongs_to_session(scope, session_id))
        {
            *active = sessions.keys().next().cloned();
        }
        // 新版本不再写入这些文件；物理删除会话时仍清理历史版本遗留数据。
        if let Err(error) =
            crate::webview_host::session_store::BrowserSessionStore::remove(session_id)
        {
            warn!(%error, session_id, "删除浏览器会话状态失败");
        }
    }
}

fn scope_belongs_to_session(scope: &str, session_id: &str) -> bool {
    scope == session_id
        || scope
            .strip_prefix("webview:")
            .and_then(|value| value.rsplit_once(':'))
            .is_some_and(|(_, scoped_session_id)| scoped_session_id == session_id)
}

impl Default for BrowserSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
