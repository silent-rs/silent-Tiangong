use tracing::{debug, warn};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewBuilder,
    WebviewUrl, Wry,
};

use crate::bridge::BRIDGE_SCRIPT;
use crate::types::{
    BrowserEvent, BrowserPageSnapshot, BrowserResponse, BrowserTab, BrowserTabsSnapshot,
    HistoryEntry, PageStatus, TabHistoryResult, TabListResponse,
};

fn webview_label(tab_id: &str) -> String {
    format!("browser-webview-{tab_id}")
}

/// 缩放下限：避免内容过小不可读
const MIN_ZOOM: f64 = 0.25;
/// 缩放上限：避免 WebKitGTK 高倍率渲染锯齿
const MAX_ZOOM: f64 = 5.0;

/// 规范化 URL 用于比较：去除末尾的 /，统一 https://
fn normalize_url_for_compare(url: &str) -> String {
    let s = url.trim_end_matches('/');
    s.to_string()
}

/// 浏览器 WebView 的共享状态
pub struct BrowserState {
    /// 每个标签页对应的独立 WebView 实例
    pub webviews: HashMap<String, Webview<Wry>>,
    /// 每个标签页的页面加载完成信号
    pub page_loaded_signals: HashMap<String, Arc<(Mutex<bool>, Condvar)>>,
    /// 每个标签页的最近一次页面快照
    pub latest_snapshots: HashMap<String, BrowserPageSnapshot>,
    /// 轮询检测的最后一次已知 URL
    pub last_known_url: String,
    /// 轮询检测的最后一次内容签名（前 500 字符）
    pub last_known_text_signature: String,
    /// 轮询线程停止信号
    pub poll_stop: Arc<std::sync::atomic::AtomicBool>,
    /// 事件消费线程停止信号
    pub event_poll_stop: Arc<std::sync::atomic::AtomicBool>,
    /// 浏览器面板是否可见（不可见时跳过页面数据读取）
    pub visible: Arc<std::sync::atomic::AtomicBool>,
    /// 已由后台事件线程读取、等待 Agent 消费的浏览器事件
    pub pending_events: Vec<BrowserEvent>,
    /// 标签列表
    pub tabs: Vec<BrowserTab>,
    /// 活跃标签 ID
    pub active_tab_id: Option<String>,
    /// 当前可见区域 (x, y, w, h)，用于标签切换时定位新 WebView
    pub browser_rect: (f64, f64, f64, f64),
    /// 全局浏览历史（所有标签页共享，持久化）
    pub global_history: Vec<HistoryEntry>,
    /// 每个标签页的浏览历史栈
    pub tab_histories: HashMap<String, Vec<HistoryEntry>>,
    /// 每个标签页当前在历史栈中的位置
    pub tab_history_indices: HashMap<String, usize>,
    /// 当前页面缩放比例，clamp 到 [0.25, 5.0]，持久化在 ~/.tiangong/browser-zoom.json
    pub zoom_factor: f64,
    /// 当前浏览器运行时绑定的对话会话 ID
    pub active_session_id: Option<String>,
}

impl BrowserState {
    fn active_webview(&self) -> Option<&Webview<Wry>> {
        let active_id = self.active_tab_id.as_ref()?;
        self.webviews.get(active_id)
    }

    fn active_page_loaded_signal(&self) -> Option<Arc<(Mutex<bool>, Condvar)>> {
        let active_id = self.active_tab_id.as_ref()?;
        self.page_loaded_signals.get(active_id).cloned()
    }
}

#[derive(Clone)]
pub struct BrowserManager {
    pub(crate) state: Arc<Mutex<BrowserState>>,
}

impl Default for BrowserManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserManager {
    pub fn new() -> Self {
        let global_history = load_global_history();
        let zoom_factor = load_zoom();
        Self {
            state: Arc::new(Mutex::new(BrowserState {
                webviews: HashMap::new(),
                page_loaded_signals: HashMap::new(),
                latest_snapshots: HashMap::new(),
                last_known_url: String::new(),
                last_known_text_signature: String::new(),
                poll_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                event_poll_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                visible: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                pending_events: Vec::new(),
                tabs: Vec::new(),
                active_tab_id: None,
                browser_rect: (0.0, 0.0, 0.0, 0.0),
                global_history,
                tab_histories: HashMap::new(),
                tab_history_indices: HashMap::new(),
                zoom_factor,
                active_session_id: None,
            })),
        }
    }

    pub fn clone_state(&self) -> Arc<Mutex<BrowserState>> {
        self.state.clone()
    }

    /// 浏览器是否已初始化（有标签即为已打开，包括 about:blank 延迟创建 WebView 的情况）
    pub fn is_open(&self) -> bool {
        self.state
            .lock()
            .map(|s| !s.tabs.is_empty())
            .unwrap_or(false)
    }

    pub fn is_visible(&self) -> bool {
        self.state
            .lock()
            .map(|s| s.visible.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false)
    }

    pub fn set_visible(&self, visible: bool) {
        if let Ok(s) = self.state.lock() {
            s.visible
                .store(visible, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// 当前页面缩放比例（来自持久化状态）
    pub fn zoom(&self) -> f64 {
        self.state.lock().map(|s| s.zoom_factor).unwrap_or(1.0)
    }

    /// 设置缩放：clamp 到 [MIN_ZOOM, MAX_ZOOM]，同步到所有 webview 并持久化，返回生效值
    pub fn set_zoom(&self, scale: f64) -> Result<f64, String> {
        let clamped = scale.clamp(MIN_ZOOM, MAX_ZOOM);
        {
            let mut s = self
                .state
                .lock()
                .map_err(|e| format!("锁 BrowserState 失败：{e}"))?;
            if (s.zoom_factor - clamped).abs() < f64::EPSILON {
                return Ok(clamped);
            }
            s.zoom_factor = clamped;
            for webview in s.webviews.values() {
                if let Err(e) = webview.set_zoom(clamped) {
                    warn!(error = %e, "webview set_zoom 失败");
                }
            }
        }
        persist_zoom(&self.state);
        Ok(clamped)
    }

    /// 重置缩放到 1.0
    pub fn reset_zoom(&self) -> Result<f64, String> {
        self.set_zoom(1.0)
    }

    /// 为指定标签创建独立的 WebView 实例
    fn create_webview_for_tab(
        app: &AppHandle<Wry>,
        tab_id: &str,
        url: &str,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    ) -> Result<Webview<Wry>, String> {
        let window = app
            .get_window("main")
            .ok_or_else(|| "主窗口未找到".to_string())?;

        let parsed_url: Url = url.parse().map_err(|e| format!("URL 解析失败：{e}"))?;
        let data_dir = browser_data_directory();
        let label = webview_label(tab_id);
        let tab_id_for_closure = tab_id.to_string();

        let state_clone_holder = {
            // 获取 manager state 用于 on_page_load 回调
            let plugin_state = app.state::<crate::BrowserPluginState>();
            plugin_state.manager.clone_state()
        };
        // 在 state_clone_holder 被 move 进 on_page_load 闭包前读出当前缩放，用于新建 webview 即时应用
        let initial_zoom = state_clone_holder
            .lock()
            .map(|s| s.zoom_factor)
            .unwrap_or(1.0);
        let app_clone = app.clone();

        let builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed_url))
            .initialization_script(BRIDGE_SCRIPT)
            .data_directory(data_dir)
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/605.1.15")
            .enable_clipboard_access()
            .devtools(true)
            .on_page_load(move |webview, payload| {
                use tauri::webview::PageLoadEvent;
                if payload.event() == PageLoadEvent::Finished {
                    {
                        let state = match state_clone_holder.lock() {
                            Ok(s) => s,
                            Err(_) => return,
                        };
                        if let Some(signal) =
                            state.page_loaded_signals.get(&tab_id_for_closure)
                        {
                            let (lock, cvar) = &**signal;
                            if let Ok(mut loaded) = lock.lock() {
                                *loaded = true;
                            }
                            cvar.notify_all();
                        }
                    }

                    let state_clone2 = state_clone_holder.clone();
                    let tab_id_in_closure = tab_id_for_closure.clone();
                    let app_for_event = app_clone.clone();
                    let _ = webview.eval_with_callback(
                        "window.__tiangong_bridge.getFullText(12000)",
                        move |result| {
                            if let Ok(data) =
                                serde_json::from_str::<serde_json::Value>(&result)
                            {
                                let title = data["title"].as_str().unwrap_or("").to_string();
                                let page_url = data["url"].as_str().unwrap_or("").to_string();
                                let text = data["text"].as_str().unwrap_or("").to_string();
                                let snapshot = BrowserPageSnapshot {
                                    title: title.clone(),
                                    url: page_url.clone(),
                                    text: text.clone(),
                                    status: PageStatus::Loaded,
                                    tabs: Vec::new(),
                                    active_tab_id: None,
                                    events: Vec::new(),
                                };
                                if let Ok(mut state) = state_clone2.lock() {
                                    state
                                        .latest_snapshots
                                        .insert(tab_id_in_closure.clone(), snapshot);
                                    if let Some(tab) =
                                        state.tabs.iter_mut().find(|t| t.id == tab_id_in_closure)
                                    {
                                        tab.url = page_url.clone();
                                        if !title.is_empty() {
                                            tab.title = title.clone();
                                        }
                                    }
                                    // 记录浏览历史
                                    let should_persist = {
                                        let active_id = state.active_tab_id.clone();
                                        if !page_url.starts_with("about:") {
                                            // 写入标签历史
                                            if let Some(active_id) = active_id {
                                                {
                                                    let tab_entries = state.tab_histories.entry(active_id.clone()).or_default();
                                                    let existing_pos = tab_entries.iter().position(|e| e.url == page_url);
                                                    match existing_pos {
                                                        Some(pos) => {
                                                            // URL 已在栈中，更新索引（不追加）
                                                            tab_entries[pos].title = if title.is_empty() { page_url.clone() } else { title.clone() };
                                                            tab_entries[pos].timestamp = std::time::SystemTime::now()
                                                                .duration_since(std::time::UNIX_EPOCH)
                                                                .unwrap_or_default()
                                                                .as_millis() as u64;
                                                            state.tab_history_indices.insert(active_id.clone(), pos);
                                                        }
                                                        None => {
                                                            tab_entries.push(HistoryEntry {
                                                                url: page_url.clone(),
                                                                title: if title.is_empty() { page_url.clone() } else { title.clone() },
                                                                timestamp: std::time::SystemTime::now()
                                                                    .duration_since(std::time::UNIX_EPOCH)
                                                                    .unwrap_or_default()
                                                                    .as_millis() as u64,
                                                            });
                                                            let idx = tab_entries.len() - 1;
                                                            state.tab_history_indices.insert(active_id.clone(), idx);
                                                        }
                                                    }
                                                }
                                            }
                                            // 写入全局历史（去重：移到末尾并更新时间戳）
                                            let pos = state.global_history.iter().position(|e| e.url == page_url);
                                            match pos {
                                                Some(i) => {
                                                    state.global_history[i].title = if title.is_empty() { page_url.clone() } else { title.clone() };
                                                    state.global_history[i].timestamp = std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap_or_default()
                                                        .as_millis() as u64;
                                                    let entry = state.global_history.remove(i);
                                                    state.global_history.push(entry);
                                                }
                                                None => {
                                                    state.global_history.push(HistoryEntry {
                                                        url: page_url.clone(),
                                                        title: if title.is_empty() { page_url.clone() } else { title.clone() },
                                                        timestamp: std::time::SystemTime::now()
                                                            .duration_since(std::time::UNIX_EPOCH)
                                                            .unwrap_or_default()
                                                            .as_millis() as u64,
                                                    });
                                                }
                                            }
                                            if state.global_history.len() > 1000 {
                                                let keep = state.global_history.len() - 800;
                                                state.global_history.drain(0..keep);
                                            }
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    drop(state);
                                    if should_persist {
                                        persist_global_history(&state_clone2);
                                    }
                                }
                                let summary: String = text.chars().take(2000).collect();
                                let _ = app_for_event.emit(
                                    "browser:page_loaded",
                                    serde_json::json!({
                                        "title": title,
                                        "url": page_url,
                                        "text": summary,
                                    }),
                                );
                            }
                        },
                    );
                    let _ = webview.eval("window.__tiangong_bridge.observer.start()");
                }
            });

        let webview = window
            .add_child(builder, LogicalPosition::new(x, y), LogicalSize::new(w, h))
            .map_err(|e| format!("创建浏览器 WebView 失败：{e}"))?;

        // 创建后立即应用当前缩放，避免首屏以 100% 渲染再跳变
        if (initial_zoom - 1.0).abs() > f64::EPSILON {
            if let Err(e) = webview.set_zoom(initial_zoom) {
                warn!(error = %e, "新建 webview 应用初始缩放失败");
            }
        }

        Ok(webview)
    }

    pub fn open(
        &self,
        app: &AppHandle<Wry>,
        url: &str,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    ) -> Result<(), String> {
        let existing_tab_webview_to_create = {
            let mut state = self.state.lock().map_err(|e| e.to_string())?;
            if !state.tabs.is_empty() {
                // 已初始化：更新活跃标签（有 WebView 则导航，无则标记 URL 等待按需创建）
                let mut webview_to_create = None;
                if let Some(matching_tab) = state
                    .tabs
                    .iter()
                    .find(|t| normalize_url_for_compare(&t.url) == normalize_url_for_compare(url))
                {
                    // 切换到已有标签
                    let matching_id = matching_tab.id.clone();
                    let old_active = state.active_tab_id.clone();
                    if old_active.as_deref() != Some(&matching_id) {
                        // 隐藏旧活跃 WebView
                        if let Some(old_id) = &old_active {
                            if let Some(old_wv) = state.webviews.get(old_id) {
                                let _ = old_wv.set_position(LogicalPosition::new(-10000, -10000));
                            }
                        }
                        // 显示目标 WebView
                        if let Some(new_wv) = state.webviews.get(&matching_id) {
                            let _ = new_wv.set_position(LogicalPosition::new(x, y));
                            let _ = new_wv.set_size(LogicalSize::new(w, h));
                        } else if url != "about:blank" {
                            webview_to_create = Some(matching_id.clone());
                        }
                        state.active_tab_id = Some(matching_id);
                    } else {
                        // 已经是当前标签，只更新位置
                        if let Some(wv) = state.active_webview() {
                            let _ = wv.set_position(LogicalPosition::new(x, y));
                            let _ = wv.set_size(LogicalSize::new(w, h));
                        } else if url != "about:blank" {
                            webview_to_create = Some(matching_id);
                        }
                    }
                } else {
                    // 无匹配 URL，导航当前活跃标签
                    if let Some(wv) = state.active_webview() {
                        let _ = wv.set_position(LogicalPosition::new(x, y));
                        let _ = wv.set_size(LogicalSize::new(w, h));
                    }
                    let parsed_url: Url = url.parse().map_err(|e| format!("URL 解析失败：{e}"))?;
                    if let Some(wv) = state.active_webview() {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _ = wv.navigate(parsed_url);
                        }));
                    } else if url != "about:blank" {
                        webview_to_create = state.active_tab_id.clone();
                    }
                    // 更新活跃标签 URL
                    let active_id = state.active_tab_id.clone();
                    if let Some(active_id) = active_id {
                        if let Some(tab) = state.tabs.iter_mut().find(|t| t.id == active_id) {
                            tab.url = url.to_string();
                        }
                    }
                }
                state.browser_rect = (x, y, w, h);
                state
                    .visible
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Some(webview_to_create)
            } else {
                None
            }
        };

        if let Some(webview_to_create) = existing_tab_webview_to_create {
            if let Some(tab_id) = webview_to_create {
                let webview = Self::create_webview_for_tab(app, &tab_id, url, x, y, w, h)?;
                let mut state = self.state.lock().map_err(|e| e.to_string())?;
                state.webviews.insert(tab_id, webview);
                drop(state);
                self.start_url_poll(app, url);
                self.start_event_poll(app);
            }
            return Ok(());
        }

        // 首次创建：创建标签 + WebView（about:blank 跳过 WebView 创建）
        let tab_id = scru128::new().to_string();
        let is_blank = url == "about:blank";

        if !is_blank {
            let webview = Self::create_webview_for_tab(app, &tab_id, url, x, y, w, h)?;
            if let Ok(mut state) = self.state.lock() {
                state.webviews.insert(tab_id.clone(), webview);
            }
        }

        if let Ok(mut state) = self.state.lock() {
            state.page_loaded_signals.insert(
                tab_id.clone(),
                Arc::new((Mutex::new(false), Condvar::new())),
            );
            // 初始化标签页历史（排除 about: 页面）
            if !url.starts_with("about:") {
                let entry = HistoryEntry {
                    url: url.to_string(),
                    title: url.to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                };
                state.tab_histories.insert(tab_id.clone(), vec![entry]);
                state.tab_history_indices.insert(tab_id.clone(), 0);
            }
            state.tabs.push(BrowserTab {
                id: tab_id.clone(),
                url: url.to_string(),
                title: String::new(),
            });
            state.active_tab_id = Some(tab_id);
            state.browser_rect = (x, y, w, h);
            state
                .visible
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }

        self.start_url_poll(app, url);
        self.start_event_poll(app);

        Ok(())
    }

    pub fn close(&self) -> Result<(), String> {
        if let Ok(mut state) = self.state.lock() {
            reset_runtime_state(&mut state, true);
            state.active_session_id = None;
            state
                .visible
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// 启动后台轮询线程，检测 webview URL 变化并发射 browser:page_loaded 事件
    fn start_url_poll(&self, app: &AppHandle<Wry>, initial_url: &str) {
        let state = self.state.clone();
        let app = app.clone();
        let stop = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.poll_stop
                .store(false, std::sync::atomic::Ordering::Relaxed);
            s.last_known_url = initial_url.to_string();
            s.poll_stop.clone()
        };
        let visible = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.visible.clone()
        };

        std::thread::Builder::new()
            .name("browser-url-poll".into())
            .spawn(move || {
                let mut tick: u32 = 0;
                let mut no_webview_ticks: u32 = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    tick += 1;
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if !visible.load(std::sync::atomic::Ordering::Relaxed) {
                        continue;
                    }
                    let current_url = {
                        let s = match state.lock() {
                            Ok(s) => s,
                            Err(e) => e.into_inner(),
                        };
                        match s.active_webview() {
                            Some(wv) => {
                                no_webview_ticks = 0;
                                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    wv.url()
                                })) {
                                    Ok(Ok(u)) => u.to_string(),
                                    _ => continue,
                                }
                            }
                            None => {
                                no_webview_ticks += 1;
                                // 无 WebView 时等待最多 30 秒（60 个 tick），超时退出
                                if no_webview_ticks > 60 {
                                    break;
                                }
                                continue;
                            }
                        }
                    };
                    let changed = {
                        let mut s = match state.lock() {
                            Ok(s) => s,
                            Err(e) => e.into_inner(),
                        };
                        if current_url != s.last_known_url {
                            s.last_known_url = current_url.clone();
                            true
                        } else {
                            false
                        }
                    };
                    if changed {
                        debug!(url = %current_url, "browser url_poll detected change");
                        // 更新活跃标签 URL（历史记录由 on_page_load 回调负责）
                        {
                            let mut s = match state.lock() {
                                Ok(s) => s,
                                Err(e) => e.into_inner(),
                            };
                            let aid = s.active_tab_id.clone();
                            if let Some(active_id) = aid {
                                if let Some(tab) = s.tabs.iter_mut().find(|t| t.id == active_id) {
                                    tab.url = current_url.clone();
                                }
                            }
                        }
                        let _ = app.emit(
                            "browser:page_loaded",
                            serde_json::json!({
                                "url": current_url,
                            }),
                        );
                    }

                    // 每 3 秒检测页面内容变化
                    if tick.is_multiple_of(6) {
                        let mgr = BrowserManager {
                            state: state.clone(),
                        };
                        if let Some(sig) = mgr.eval_with_result(
                            "(function(){try{return(document.body.innerText||'').substring(0,500).trim()}catch(e){return''}})()"
                        ) {
                            let content_changed = {
                                let mut s = match state.lock() {
                                    Ok(s) => s,
                                    Err(e) => e.into_inner(),
                                };
                                if sig != s.last_known_text_signature && !sig.is_empty() {
                                    s.last_known_text_signature = sig;
                                    true
                                } else {
                                    false
                                }
                            };
                            if content_changed {
                                debug!("browser url_poll detected content change");
                                let mgr2 = BrowserManager {
                                    state: state.clone(),
                                };
                                if let Some(raw) = mgr2.eval_with_result(
                                    "(function(){try{var t=window.__tiangong_bridge.getFullText(12000);return JSON.stringify(t)}catch(e){return '{}'}})()"
                                ) {
                                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                                        let title = data["title"].as_str().unwrap_or("").to_string();
                                        let url = data["url"].as_str().unwrap_or("").to_string();
                                        let text: String =
                                            data["text"].as_str().unwrap_or("").chars().take(2000).collect();
                                        let _ = app.emit(
                                            "browser:page_loaded",
                                            serde_json::json!({
                                                "title": title,
                                                "url": url,
                                                "text": text,
                                            }),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                debug!("browser url_poll thread exiting");
            })
            .expect("failed to spawn browser URL poll thread");
    }

    pub fn hide(&self) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        for wv in state.webviews.values() {
            let _ = wv.set_size(LogicalSize::new(0.0, 0.0));
            let _ = wv.set_position(LogicalPosition::new(-10000, -10000));
        }
        state
            .visible
            .store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn go_back(&self) -> Result<(), String> {
        self.eval("history.back()")
    }

    pub fn go_forward(&self) -> Result<(), String> {
        self.eval("history.forward()")
    }

    pub fn set_position(&self, x: f64, y: f64) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(wv) = state.active_webview() {
            wv.set_position(LogicalPosition::new(x, y))
                .map_err(|e| format!("设置浏览器位置失败：{e}"))?;
        }
        state.browser_rect.0 = x;
        state.browser_rect.1 = y;
        Ok(())
    }

    /// 启动事件消费线程
    fn start_event_poll(&self, app: &AppHandle<Wry>) {
        let state = self.state.clone();
        let app = app.clone();
        let stop = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.event_poll_stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
            s.event_poll_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            s.event_poll_stop.clone()
        };
        let visible = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.visible.clone()
        };

        std::thread::Builder::new()
            .name("browser-event-poll".into())
            .spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if !visible.load(std::sync::atomic::Ordering::Relaxed) {
                        continue;
                    }

                    let mgr = BrowserManager {
                        state: state.clone(),
                    };
                    if let Some(raw) = mgr.eval_with_result(
                        "(function(){try{return window.__tiangong_bridge.observer.drainAllEvents()}catch(e){return[]}})()",
                    ) {
                        if raw == "[]" || raw.is_empty() {
                            continue;
                        }
                        if let Ok(events) =
                            serde_json::from_str::<Vec<crate::types::BrowserEvent>>(&raw)
                        {
                            if !events.is_empty() {
                                if let Ok(mut s) = state.lock() {
                                    s.pending_events.extend(events.clone());
                                    if s.pending_events.len() > 200 {
                                        let keep_from = s.pending_events.len() - 100;
                                        s.pending_events.drain(0..keep_from);
                                    }
                                }
                                let _ = app.emit("browser:events", &events);
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn browser event poll thread");
    }

    pub fn set_size(&self, w: f64, h: f64) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(wv) = state.active_webview() {
            wv.set_size(LogicalSize::new(w, h))
                .map_err(|e| format!("设置浏览器尺寸失败：{e}"))?;
        }
        state.browser_rect.2 = w;
        state.browser_rect.3 = h;
        Ok(())
    }

    pub fn navigate(&self, url: &str) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(signal) = state.active_page_loaded_signal() {
            let (lock, _cvar) = &*signal;
            if let Ok(mut loaded) = lock.lock() {
                *loaded = false;
            }
        }
        if let Some(wv) = state.active_webview() {
            let parsed_url: Url = url.parse().map_err(|e| format!("URL 解析失败：{e}"))?;
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wv.navigate(parsed_url)));
            result
                .map_err(|_| "WebView 导航内部错误".to_string())?
                .map_err(|e| format!("导航失败：{e}"))?;
        }
        Ok(())
    }

    /// 导航到 URL，自动处理所有场景：
    /// 1. 浏览器未打开 → 在屏幕外创建 WebView，等待前端设置正确位置
    /// 2. 已有匹配 URL 的标签 → 切换到该标签
    /// 3. 当前活跃标签无 WebView → 创建 WebView
    /// 4. 正常导航当前活跃 WebView
    pub fn navigate_with_app(&self, app: &AppHandle<Wry>, url: &str) -> Result<(), String> {
        // Agent 主动导航时恢复可见状态，使数据读取路径可用
        self.set_visible(true);
        // 场景 1：浏览器完全未打开 → 在屏幕外创建，前端会通过 set_position 设置正确位置
        if !self.is_open() {
            if let Some((_, _, w, h)) = default_browser_rect(app) {
                return self.open(app, url, -10000.0, -10000.0, w, h);
            }
            return Err("无法确定浏览器位置".to_string());
        }

        let (matching_tab_id, needs_create, active_id, rect) = {
            let state = self.state.lock().map_err(|e| e.to_string())?;

            // 场景 2：检查已有匹配 URL 的标签
            let matching = state
                .tabs
                .iter()
                .find(|t| normalize_url_for_compare(&t.url) == normalize_url_for_compare(url))
                .map(|t| t.id.clone());

            let needs_create = state
                .active_tab_id
                .as_ref()
                .map(|id| !state.webviews.contains_key(id))
                .unwrap_or(false);

            (
                matching,
                needs_create,
                state.active_tab_id.clone(),
                state.browser_rect,
            )
        };

        // 场景 2：切换到已有标签
        if let Some(matching_id) = matching_tab_id {
            let mut state = self.state.lock().map_err(|e| e.to_string())?;
            if state.active_tab_id.as_deref() != Some(&matching_id) {
                if let Some(old_id) = &state.active_tab_id {
                    if let Some(old_wv) = state.webviews.get(old_id) {
                        let _ = old_wv.set_position(LogicalPosition::new(-10000, -10000));
                    }
                }
                if let Some(new_wv) = state.webviews.get(&matching_id) {
                    let _ = new_wv.set_position(LogicalPosition::new(rect.0, rect.1));
                    let _ = new_wv.set_size(LogicalSize::new(rect.2, rect.3));
                }
                state.active_tab_id = Some(matching_id);
            }
            return Ok(());
        }

        // 更新活跃标签 URL
        {
            let mut state = self.state.lock().map_err(|e| e.to_string())?;
            if let Some(ref id) = active_id {
                if let Some(tab) = state.tabs.iter_mut().find(|t| t.id == *id) {
                    tab.url = url.to_string();
                }
            }
        }

        // 场景 3：活跃标签无 WebView，创建一个
        if needs_create {
            if let Some(tab_id) = active_id {
                let webview = Self::create_webview_for_tab(
                    app, &tab_id, url, rect.0, rect.1, rect.2, rect.3,
                )?;
                let mut state = self.state.lock().map_err(|e| e.to_string())?;
                state.webviews.insert(tab_id.clone(), webview);
                // 确保 WebView 在显示区内
                if let Some(wv) = state.webviews.get(&tab_id) {
                    let _ = wv.set_position(LogicalPosition::new(rect.0, rect.1));
                    let _ = wv.set_size(LogicalSize::new(rect.2, rect.3));
                }
                drop(state);
                self.start_url_poll(app, url);
                self.start_event_poll(app);
                return Ok(());
            }
        }

        // 场景 4：正常导航
        self.navigate(url)
    }

    pub fn load_html(&self, html: &str) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(signal) = state.active_page_loaded_signal() {
            let (lock, _cvar) = &*signal;
            if let Ok(mut loaded) = lock.lock() {
                *loaded = false;
            }
        }
        if let Some(wv) = state.active_webview() {
            let encoded = base64_url::encode(html.as_bytes());
            let data_url = format!("data:text/html;base64,{encoded}");
            let parsed_url: Url = data_url
                .parse()
                .map_err(|e| format!("data URL 构造失败：{e}"))?;
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wv.navigate(parsed_url)));
            result
                .map_err(|_| "WebView 导航内部错误".to_string())?
                .map_err(|e| format!("加载 HTML 失败：{e}"))?;
        }
        Ok(())
    }

    pub fn eval(&self, js: &str) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(wv) = state.active_webview() {
            wv.eval(js).map_err(|e| format!("执行 JS 失败：{e}"))?;
        }
        Ok(())
    }

    pub(crate) fn eval_with_result(&self, js: &str) -> Option<String> {
        let (sender, rx) = std::sync::mpsc::channel();
        let tx = Arc::new(std::sync::Mutex::new(Some(sender)));
        {
            let state = self.state.lock().ok()?;
            let webview = state.active_webview()?;
            webview
                .eval_with_callback(js, move |result| {
                    if let Ok(mut guard) = tx.lock() {
                        if let Some(tx) = guard.take() {
                            let _ = tx.send(result);
                        }
                    }
                })
                .ok()?;
        }
        rx.recv_timeout(Duration::from_secs(15)).ok()
    }

    pub(crate) fn drain_events(&self) -> Vec<BrowserEvent> {
        let mut live_count = 0usize;
        let mut events = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(e) => e.into_inner(),
            };
            std::mem::take(&mut state.pending_events)
        };
        let cached_count = events.len();

        if let Some(raw) = self.eval_with_result(
            "(function(){try{return window.__tiangong_bridge.observer.drainAllEvents()}catch(e){return[]}})()",
        ) {
            if raw != "[]" && !raw.is_empty() {
                if let Ok(mut current) = serde_json::from_str::<Vec<BrowserEvent>>(&raw) {
                    live_count = current.len();
                    events.append(&mut current);
                }
            }
        }

        events.sort_by_key(|event| match event {
            BrowserEvent::DialogOpened { timestamp, .. }
            | BrowserEvent::DialogClosed { timestamp }
            | BrowserEvent::ContentChanged { timestamp, .. }
            | BrowserEvent::UserClick { timestamp, .. }
            | BrowserEvent::UserInput { timestamp, .. }
            | BrowserEvent::UserNavigation { timestamp, .. }
            | BrowserEvent::NetworkResponse { timestamp, .. } => *timestamp,
        });
        let network_count = events
            .iter()
            .filter(|event| matches!(event, BrowserEvent::NetworkResponse { .. }))
            .count();
        debug!(
            cached_count,
            live_count,
            total_count = events.len(),
            network_count,
            "browser manager drain_events"
        );
        events
    }

    pub fn ack_events(&self, events: &[BrowserEvent]) -> usize {
        if events.is_empty() {
            return 0;
        }
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(e) => e.into_inner(),
        };
        let before = state.pending_events.len();
        state
            .pending_events
            .retain(|event| !events.iter().any(|acked| acked == event));
        let removed = before.saturating_sub(state.pending_events.len());
        debug!(
            ack_count = events.len(),
            removed,
            pending_len = state.pending_events.len(),
            "browser manager ack_events"
        );
        removed
    }

    fn wait_for_page_load(&self, timeout_ms: u64) -> bool {
        let page_loaded = {
            let state = match self.state.lock() {
                Ok(s) => s,
                Err(_) => return false,
            };
            state.active_page_loaded_signal()
        };
        let Some(page_loaded) = page_loaded else {
            return false;
        };
        let (lock, cvar) = &*page_loaded;
        let start = std::time::Instant::now();
        let mut guard = match lock.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if *guard {
            return true;
        }
        loop {
            let remaining = timeout_ms.saturating_sub(start.elapsed().as_millis() as u64);
            if remaining == 0 {
                return *guard;
            }
            let result = cvar.wait_timeout(guard, Duration::from_millis(remaining));
            match result {
                Ok((g, _)) => {
                    guard = g;
                    if *guard {
                        return true;
                    }
                }
                Err(_) => return false,
            }
        }
    }

    fn wait_for_content_ready(&self, timeout_ms: u64) {
        let start = std::time::Instant::now();
        let check_interval = Duration::from_millis(500);
        let timeout = Duration::from_millis(timeout_ms);
        let mut last_len: usize = 0;
        let mut stable_count: usize = 0;
        let mut content_grew = false;

        loop {
            let text_len = self
                .eval_with_result("(function(){if(!document.body)return 0;var c=document.body.cloneNode(true);var r=c.querySelectorAll('script,style,noscript');for(var i=0;i<r.length;i++)r[i].parentNode.removeChild(r[i]);return(c.textContent||'').length})()")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);

            if text_len > last_len {
                content_grew = true;
                stable_count = 0;
            } else {
                stable_count += 1;
            }
            last_len = text_len;

            if text_len > 1000 && stable_count >= 2 {
                break;
            }
            if content_grew && stable_count >= 3 {
                break;
            }
            if start.elapsed() >= timeout {
                break;
            }

            std::thread::sleep(check_interval);
        }
    }

    pub fn fetch_page_content(
        &self,
        url: &str,
        max_chars: usize,
        should_navigate: bool,
    ) -> BrowserResponse {
        let error_response = |err: String| BrowserResponse {
            ok: false,
            title: String::new(),
            content: String::new(),
            final_url: url.to_string(),
            error: Some(err),
        };

        if should_navigate {
            if let Err(err) = self.navigate(url) {
                return error_response(err);
            }
        }

        let t0 = std::time::Instant::now();
        let loaded = self.wait_for_page_load(15_000);
        debug!(
            loaded,
            elapsed_ms = t0.elapsed().as_millis() as u64,
            "browser wait_for_page_load"
        );

        let t1 = std::time::Instant::now();
        self.wait_for_content_ready(15_000);
        debug!(
            elapsed_ms = t1.elapsed().as_millis() as u64,
            "browser wait_for_content_ready"
        );

        let result = self.eval_with_result(&format!(
            "window.__tiangong_bridge.getFullText({max_chars})"
        ));

        match result {
            Some(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(data) => {
                    let title = data["title"].as_str().unwrap_or("").to_string();
                    let content = data["text"].as_str().unwrap_or("").to_string();
                    let final_url = data["url"].as_str().unwrap_or(url).to_string();
                    BrowserResponse {
                        ok: true,
                        title,
                        content,
                        final_url,
                        error: None,
                    }
                }
                Err(e) => {
                    warn!(error = %e, "browser JSON parse error");
                    error_response("解析页面内容失败".to_string())
                }
            },
            None => error_response("获取页面内容超时".to_string()),
        }
    }

    pub fn get_snapshot(&self) -> Option<BrowserPageSnapshot> {
        let state = self.state.lock().ok()?;
        let active_id = state.active_tab_id.as_ref()?;
        state.latest_snapshots.get(active_id).cloned()
    }

    pub fn current_snapshot_without_events(&self, max_chars: usize) -> Option<BrowserPageSnapshot> {
        if !self.is_visible() {
            return None;
        }
        let raw = self.eval_with_result(&format!(
            "(function(){{try{{var t=window.__tiangong_bridge.getFullText({max_chars});var a=window.__tiangong_bridge.annotation.getAnnotations();if(a&&a.count>0){{t.text+='\\n\\n[页面批注] '+JSON.stringify(a.annotations);}}return JSON.stringify(t);}}catch(e){{return ''}}}})()"
        ))?;
        let data = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
        let (tabs, active_tab_id) = {
            let state = match self.state.lock() {
                Ok(state) => state,
                Err(e) => e.into_inner(),
            };
            (state.tabs.clone(), state.active_tab_id.clone())
        };
        Some(BrowserPageSnapshot {
            title: data["title"].as_str().unwrap_or("").to_string(),
            url: data["url"].as_str().unwrap_or("").to_string(),
            text: data["text"].as_str().unwrap_or("").to_string(),
            status: PageStatus::Loaded,
            tabs,
            active_tab_id,
            events: Vec::new(),
        })
    }

    pub fn tab_list(&self) -> Vec<BrowserTab> {
        self.state
            .lock()
            .map(|s| s.tabs.clone())
            .unwrap_or_default()
    }

    pub fn tab_list_with_active(&self) -> TabListResponse {
        match self.state.lock() {
            Ok(s) => TabListResponse {
                tabs: s.tabs.clone(),
                active_tab_id: s.active_tab_id.clone(),
            },
            Err(e) => {
                let s = e.into_inner();
                TabListResponse {
                    tabs: s.tabs.clone(),
                    active_tab_id: s.active_tab_id.clone(),
                }
            }
        }
    }

    pub fn snapshot_tabs(&self) -> BrowserTabsSnapshot {
        match self.state.lock() {
            Ok(s) => BrowserTabsSnapshot {
                session_id: s.active_session_id.clone(),
                tabs: s.tabs.clone(),
                active_tab_id: s.active_tab_id.clone(),
            },
            Err(e) => {
                let s = e.into_inner();
                BrowserTabsSnapshot {
                    session_id: s.active_session_id.clone(),
                    tabs: s.tabs.clone(),
                    active_tab_id: s.active_tab_id.clone(),
                }
            }
        }
    }

    pub fn switch_session(
        &self,
        app: &AppHandle<Wry>,
        session_id: &str,
        tabs_to_restore: Vec<BrowserTab>,
        active_tab_id: Option<String>,
    ) -> Result<BrowserTabsSnapshot, String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        if state.active_session_id.as_deref() == Some(session_id)
            && state.tabs == tabs_to_restore
            && state.active_tab_id == active_tab_id
        {
            let active_tab = state
                .active_tab_id
                .as_ref()
                .and_then(|id| state.tabs.iter().find(|tab| tab.id == *id).cloned());
            let rect = state.browser_rect;
            let needs_webview = active_tab.as_ref().is_some_and(|tab| {
                !tab.url.starts_with("about:") && !state.webviews.contains_key(&tab.id)
            });
            if !needs_webview {
                return Ok(BrowserTabsSnapshot {
                    session_id: state.active_session_id.clone(),
                    tabs: state.tabs.clone(),
                    active_tab_id: state.active_tab_id.clone(),
                });
            }

            if let Some(tab) = active_tab {
                drop(state);
                let webview = Self::create_webview_for_tab(
                    app, &tab.id, &tab.url, rect.0, rect.1, rect.2, rect.3,
                )?;
                let mut state = self.state.lock().map_err(|e| e.to_string())?;
                state.webviews.insert(tab.id.clone(), webview);
                drop(state);
                self.start_url_poll(app, &tab.url);
                self.start_event_poll(app);
                let state = self.state.lock().map_err(|e| e.to_string())?;
                return Ok(BrowserTabsSnapshot {
                    session_id: state.active_session_id.clone(),
                    tabs: state.tabs.clone(),
                    active_tab_id: state.active_tab_id.clone(),
                });
            }

            return Ok(BrowserTabsSnapshot {
                session_id: state.active_session_id.clone(),
                tabs: state.tabs.clone(),
                active_tab_id: state.active_tab_id.clone(),
            });
        }

        reset_runtime_state(&mut state, true);
        state.tabs = tabs_to_restore;
        state.active_tab_id = resolve_active_browser_tab(&state.tabs, active_tab_id);
        restore_tab_runtime_metadata(&mut state);
        state.active_session_id = Some(session_id.to_string());
        state
            .visible
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let active_tab = state
            .active_tab_id
            .as_ref()
            .and_then(|id| state.tabs.iter().find(|tab| tab.id == *id).cloned());
        let rect = state.browser_rect;
        drop(state);

        if let Some(tab) = active_tab {
            if !tab.url.starts_with("about:") {
                let webview = Self::create_webview_for_tab(
                    app, &tab.id, &tab.url, rect.0, rect.1, rect.2, rect.3,
                )?;
                let mut state = self.state.lock().map_err(|e| e.to_string())?;
                state.webviews.insert(tab.id.clone(), webview);
                drop(state);
                self.start_url_poll(app, &tab.url);
                self.start_event_poll(app);
            }
        }

        let state = self.state.lock().map_err(|e| e.to_string())?;

        Ok(BrowserTabsSnapshot {
            session_id: state.active_session_id.clone(),
            tabs: state.tabs.clone(),
            active_tab_id: state.active_tab_id.clone(),
        })
    }

    pub fn tab_new(&self, app: &AppHandle<Wry>, url: &str) -> Result<String, String> {
        let tab_id = scru128::new().to_string();
        let is_blank = url == "about:blank";

        let rect = {
            let mut state = self.state.lock().map_err(|e| e.to_string())?;
            // 隐藏旧活跃 WebView
            if let Some(old_id) = &state.active_tab_id {
                if let Some(old_wv) = state.webviews.get(old_id) {
                    let _ = old_wv.set_position(LogicalPosition::new(-10000, -10000));
                }
            }
            let rect = state.browser_rect;
            state.page_loaded_signals.insert(
                tab_id.clone(),
                Arc::new((Mutex::new(false), Condvar::new())),
            );
            // 初始化标签页历史（排除 about: 页面）
            if !url.starts_with("about:") {
                let entry = HistoryEntry {
                    url: url.to_string(),
                    title: url.to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                };
                state.tab_histories.insert(tab_id.clone(), vec![entry]);
                state.tab_history_indices.insert(tab_id.clone(), 0);
            }
            state.tabs.push(BrowserTab {
                id: tab_id.clone(),
                url: url.to_string(),
                title: String::new(),
            });
            state.active_tab_id = Some(tab_id.clone());
            rect
        };

        // about:blank 不创建 WebView（WKWebView 对 about:blank 的 URL() 返回 None，
        // 会导致 Tauri 权限检查内部 panic），延迟到 navigate 时按需创建
        if !is_blank {
            let webview =
                Self::create_webview_for_tab(app, &tab_id, url, rect.0, rect.1, rect.2, rect.3)?;

            let mut state = self.state.lock().map_err(|e| e.to_string())?;
            state.webviews.insert(tab_id.clone(), webview);
        }
        Ok(tab_id)
    }

    pub fn tab_switch(&self, tab_id: &str) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;

        // 检查标签是否存在
        state
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .ok_or_else(|| format!("标签 {tab_id} 不存在"))?;

        if state.active_tab_id.as_deref() == Some(tab_id) {
            return Ok(());
        }

        let rect = state.browser_rect;

        // 隐藏旧活跃 WebView
        if let Some(old_id) = &state.active_tab_id {
            if let Some(old_wv) = state.webviews.get(old_id) {
                let _ = old_wv.set_position(LogicalPosition::new(-10000, -10000));
            }
        }

        // 显示目标 WebView（about:blank 标签可能没有 WebView，属于正常情况）
        if let Some(new_wv) = state.webviews.get(tab_id) {
            let _ = new_wv.set_position(LogicalPosition::new(rect.0, rect.1));
            let _ = new_wv.set_size(LogicalSize::new(rect.2, rect.3));
        }

        state.active_tab_id = Some(tab_id.to_string());
        Ok(())
    }

    pub fn tab_close(&self, tab_id: &str) -> Result<(), String> {
        let (was_active, closed_pos) = {
            let mut state = self.state.lock().map_err(|e| e.to_string())?;
            let pos = state
                .tabs
                .iter()
                .position(|t| t.id == tab_id)
                .ok_or_else(|| format!("标签 {tab_id} 不存在"))?;

            state.tabs.remove(pos);
            // 关闭对应的 WebView
            if let Some(webview) = state.webviews.remove(tab_id) {
                let _ = webview.close();
            }
            state.page_loaded_signals.remove(tab_id);
            state.latest_snapshots.remove(tab_id);
            // 清除该标签页的历史
            state.tab_histories.remove(tab_id);
            state.tab_history_indices.remove(tab_id);
            let was_active = state.active_tab_id.as_deref() == Some(tab_id);
            (was_active, pos)
        };

        if was_active {
            let mut state = self.state.lock().map_err(|e| e.to_string())?;
            if state.tabs.is_empty() {
                // 最后一个 tab 关闭，完全关闭浏览器
                state
                    .poll_stop
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                state
                    .event_poll_stop
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                state.page_loaded_signals.clear();
                state.latest_snapshots.clear();
                state.last_known_url.clear();
                state.last_known_text_signature.clear();
                state.pending_events.clear();
                state.active_tab_id = None;
                state.tab_histories.clear();
                state.tab_history_indices.clear();
            } else {
                // 切换到关闭位置处的相邻标签
                let new_pos = closed_pos.min(state.tabs.len().saturating_sub(1));
                let new_id = state.tabs[new_pos].id.clone();
                let rect = state.browser_rect;

                // 显示新活跃标签的 WebView
                if let Some(new_wv) = state.webviews.get(&new_id) {
                    let _ = new_wv.set_position(LogicalPosition::new(rect.0, rect.1));
                    let _ = new_wv.set_size(LogicalSize::new(rect.2, rect.3));
                }
                state.active_tab_id = Some(new_id);
            }
        }
        Ok(())
    }

    /// 更新活跃标签的 URL 和标题
    pub fn update_active_tab(&self, url: &str, title: &str) {
        if let Ok(mut state) = self.state.lock() {
            let active_id = state.active_tab_id.clone();
            if let Some(active_id) = active_id {
                if let Some(tab) = state.tabs.iter_mut().find(|t| t.id == active_id) {
                    tab.url = url.to_string();
                    if !title.is_empty() {
                        tab.title = title.to_string();
                    }
                }
            }
        }
    }

    /// 获取当前活跃标签的 WebView URL
    pub fn current_url(&self) -> Option<String> {
        let state = self.state.lock().ok()?;
        let wv = state.active_webview()?;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wv.url())) {
            Ok(Ok(u)) => Some(u.to_string()),
            _ => None,
        }
    }

    /// 记录 URL 访问到活跃标签历史和全局历史
    pub fn record_history(&self, url: &str, title: &str) {
        let should_persist = {
            let mut state = match self.state.lock() {
                Ok(s) => s,
                Err(e) => e.into_inner(),
            };
            let entry = HistoryEntry {
                url: url.to_string(),
                title: if title.is_empty() {
                    url.to_string()
                } else {
                    title.to_string()
                },
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            };

            // 写入活跃标签历史
            let active_id = state.active_tab_id.clone();
            if let Some(ref aid) = active_id {
                let tab_entries = state.tab_histories.entry(aid.clone()).or_default();
                // 去重：与最新条目 URL 相同时跳过
                if tab_entries.last().map(|e| e.url.as_str()) != Some(url) {
                    tab_entries.push(entry.clone());
                    // 标签历史上限 200 条
                    if tab_entries.len() > 200 {
                        let keep_from = tab_entries.len() - 160;
                        tab_entries.drain(0..keep_from);
                    }
                }
            }
            // 更新标签历史索引
            if let Some(ref aid) = active_id {
                let idx = state
                    .tab_histories
                    .get(aid)
                    .map(|te| te.len() - 1)
                    .unwrap_or(0);
                state.tab_history_indices.insert(aid.clone(), idx);
            }

            // 写入全局历史（去重：移到末尾并更新时间戳）
            let pos = state.global_history.iter().position(|e| e.url == url);
            match pos {
                Some(i) => {
                    state.global_history[i].title = entry.title.clone();
                    state.global_history[i].timestamp = entry.timestamp;
                    let moved = state.global_history.remove(i);
                    state.global_history.push(moved);
                    true
                }
                None => {
                    state.global_history.push(entry);
                    if state.global_history.len() > 1000 {
                        let keep_from = state.global_history.len() - 800;
                        state.global_history.drain(0..keep_from);
                    }
                    true
                }
            }
        };
        if should_persist {
            persist_global_history(&self.state);
        }
    }

    /// 获取标签页浏览历史
    pub fn get_tab_history(&self, tab_id: Option<&str>) -> Option<TabHistoryResult> {
        let state = self.state.lock().ok()?;
        let target_id = tab_id
            .map(|s| s.to_string())
            .or_else(|| state.active_tab_id.clone())?;
        let entries = state.tab_histories.get(&target_id)?.clone();
        let current_index = state
            .tab_history_indices
            .get(&target_id)
            .copied()
            .unwrap_or(0) as i32;
        Some(TabHistoryResult {
            tab_id: target_id,
            entries,
            current_index,
        })
    }

    /// 获取全局浏览历史（分页，最新在前）
    pub fn get_global_history(&self, offset: usize, limit: usize) -> Vec<HistoryEntry> {
        let state = match self.state.lock() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        };
        let total = state.global_history.len();
        if offset >= total {
            return Vec::new();
        }
        // 倒序切片：offset=0 取最后 limit 条
        let end = total.saturating_sub(offset);
        let start = end.saturating_sub(limit);
        state.global_history[start..end]
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    /// 清空全局浏览历史
    pub fn clear_global_history(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.global_history.clear();
        }
        persist_global_history(&self.state);
    }

    /// 删除全局历史中指定 URL 的条目
    pub fn delete_global_history_entry(&self, url: &str) {
        let should_persist = {
            let mut state = match self.state.lock() {
                Ok(s) => s,
                Err(e) => e.into_inner(),
            };
            let before = state.global_history.len();
            state.global_history.retain(|e| e.url != url);
            before != state.global_history.len()
        };
        if should_persist {
            persist_global_history(&self.state);
        }
    }

    /// 清除指定标签页的历史
    pub fn clear_tab_history(&self, tab_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.tab_histories.remove(tab_id);
            state.tab_history_indices.remove(tab_id);
        }
    }

    /// 初始化标签页历史
    pub fn init_tab_history(&self, tab_id: &str, url: &str, title: &str) {
        if let Ok(mut state) = self.state.lock() {
            let entry = HistoryEntry {
                url: url.to_string(),
                title: if title.is_empty() {
                    url.to_string()
                } else {
                    title.to_string()
                },
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            };
            state.tab_histories.insert(tab_id.to_string(), vec![entry]);
            state.tab_history_indices.insert(tab_id.to_string(), 0);
        }
    }
}

fn reset_runtime_state(state: &mut BrowserState, close_webviews: bool) {
    state
        .poll_stop
        .store(true, std::sync::atomic::Ordering::Relaxed);
    state
        .event_poll_stop
        .store(true, std::sync::atomic::Ordering::Relaxed);
    state.poll_stop = Arc::new(std::sync::atomic::AtomicBool::new(true));
    state.event_poll_stop = Arc::new(std::sync::atomic::AtomicBool::new(true));

    if close_webviews {
        for (_, webview) in state.webviews.drain() {
            let _ = webview.close();
        }
    } else {
        state.webviews.clear();
    }

    for signal in state.page_loaded_signals.values() {
        let (lock, cvar) = &**signal;
        if let Ok(mut loaded) = lock.lock() {
            *loaded = false;
        }
        cvar.notify_all();
    }

    state.page_loaded_signals.clear();
    state.latest_snapshots.clear();
    state.last_known_url.clear();
    state.last_known_text_signature.clear();
    state.pending_events.clear();
    state.tabs.clear();
    state.active_tab_id = None;
    state.tab_histories.clear();
    state.tab_history_indices.clear();
}

fn resolve_active_browser_tab(
    tabs: &[BrowserTab],
    active_tab_id: Option<String>,
) -> Option<String> {
    active_tab_id
        .filter(|id| tabs.iter().any(|tab| tab.id == *id))
        .or_else(|| tabs.first().map(|tab| tab.id.clone()))
}

fn restore_tab_runtime_metadata(state: &mut BrowserState) {
    for tab in &state.tabs {
        state.page_loaded_signals.insert(
            tab.id.clone(),
            Arc::new((Mutex::new(false), Condvar::new())),
        );
        if !tab.url.starts_with("about:") {
            let title = if tab.title.is_empty() {
                tab.url.clone()
            } else {
                tab.title.clone()
            };
            state.tab_histories.insert(
                tab.id.clone(),
                vec![HistoryEntry {
                    url: tab.url.clone(),
                    title,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                }],
            );
            state.tab_history_indices.insert(tab.id.clone(), 0);
        }
    }
}

fn global_history_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".tiangong")
        .join("browser-history.json")
}

fn load_global_history() -> Vec<HistoryEntry> {
    let path = global_history_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn persist_global_history(state: &Arc<Mutex<BrowserState>>) {
    let entries = {
        let state = match state.lock() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        };
        state.global_history.clone()
    };
    let path = global_history_path();
    if let Ok(content) = serde_json::to_string(&entries) {
        let _ = std::fs::write(path, content);
    }
}

fn zoom_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".tiangong")
        .join("browser-zoom.json")
}

fn load_zoom() -> f64 {
    let path = zoom_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str::<f64>(&content).unwrap_or(1.0),
        Err(_) => 1.0,
    }
}

fn persist_zoom(state: &Arc<Mutex<BrowserState>>) {
    let zoom = {
        let state = match state.lock() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        };
        state.zoom_factor
    };
    let path = zoom_path();
    if let Ok(content) = serde_json::to_string(&zoom) {
        let _ = std::fs::write(path, content);
    }
}

fn browser_data_directory() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".tiangong").join("browser-data")
}

pub fn default_browser_rect(app: &AppHandle<Wry>) -> Option<(f64, f64, f64, f64)> {
    let window = app.get_window("main")?;
    let size = window.inner_size().ok()?;
    let scale = window.scale_factor().unwrap_or(1.0);
    let w = size.width as f64 / scale;
    let h = size.height as f64 / scale;
    let browser_w = w * 0.5;
    Some((w - browser_w, 0.0, browser_w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_events_removes_only_injected_events() {
        let manager = BrowserManager::new();
        let first = BrowserEvent::NetworkResponse {
            timestamp: 1,
            url: "/api/a".to_string(),
            method: "POST".to_string(),
            status: 200,
            detail: "{}".to_string(),
        };
        let second = BrowserEvent::NetworkResponse {
            timestamp: 2,
            url: "/api/b".to_string(),
            method: "POST".to_string(),
            status: 200,
            detail: "{}".to_string(),
        };
        {
            let mut state = manager.state.lock().unwrap();
            state.pending_events.push(first.clone());
            state.pending_events.push(second.clone());
        }

        let removed = manager.ack_events(std::slice::from_ref(&first));

        assert_eq!(removed, 1);
        let state = manager.state.lock().unwrap();
        assert_eq!(state.pending_events, vec![second]);
    }
}
