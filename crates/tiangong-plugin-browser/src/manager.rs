use tracing::{debug, warn};

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewBuilder,
    WebviewUrl, Wry,
};

use crate::bridge::BRIDGE_SCRIPT;
use crate::types::{BrowserPageSnapshot, BrowserResponse, BrowserTab, PageStatus};

const BROWSER_WEBVIEW_LABEL: &str = "browser-webview";

/// 浏览器 WebView 的共享状态
pub struct BrowserState {
    pub webview: Option<Webview<Wry>>,
    /// 页面加载完成信号
    pub page_loaded: Arc<(Mutex<bool>, Condvar)>,
    /// 最近一次页面快照
    pub latest_snapshot: Option<BrowserPageSnapshot>,
    /// 轮询检测的最后一次已知 URL
    pub last_known_url: String,
    /// 轮询线程停止信号
    pub poll_stop: Arc<std::sync::atomic::AtomicBool>,
    /// 标签列表
    pub tabs: Vec<BrowserTab>,
    /// 活跃标签 ID
    pub active_tab_id: Option<String>,
}

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
        Self {
            state: Arc::new(Mutex::new(BrowserState {
                webview: None,
                page_loaded: Arc::new((Mutex::new(false), Condvar::new())),
                latest_snapshot: None,
                last_known_url: String::new(),
                poll_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                tabs: Vec::new(),
                active_tab_id: None,
            })),
        }
    }

    pub fn clone_state(&self) -> Arc<Mutex<BrowserState>> {
        self.state.clone()
    }

    pub fn is_open(&self) -> bool {
        self.state
            .lock()
            .map(|s| s.webview.is_some())
            .unwrap_or(false)
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
        {
            let mut state = self.state.lock().map_err(|e| e.to_string())?;
            if let Some(webview) = &state.webview {
                webview
                    .set_position(LogicalPosition::new(x, y))
                    .map_err(|e| format!("恢复浏览器位置失败：{e}"))?;
                webview
                    .set_size(LogicalSize::new(w, h))
                    .map_err(|e| format!("恢复浏览器尺寸失败：{e}"))?;
                // WebView 已存在：确保有标签并导航到目标 URL
                if state.tabs.is_empty() {
                    let tab_id = scru128::new().to_string();
                    state.tabs.push(BrowserTab {
                        id: tab_id.clone(),
                        url: url.to_string(),
                        title: String::new(),
                    });
                    state.active_tab_id = Some(tab_id);
                }
                drop(state);
                self.navigate(url)?;
                return Ok(());
            }
        }

        let window = app
            .get_window("main")
            .ok_or_else(|| "主窗口未找到".to_string())?;

        let parsed_url: Url = url.parse().map_err(|e| format!("URL 解析失败：{e}"))?;

        let data_dir = browser_data_directory();
        let state_clone = self.state.clone();
        let app_clone = app.clone();

        let builder = WebviewBuilder::new(BROWSER_WEBVIEW_LABEL, WebviewUrl::External(parsed_url))
            .initialization_script(BRIDGE_SCRIPT)
            .data_directory(data_dir)
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/605.1.15")
            .enable_clipboard_access()
            .devtools(true)
            .on_page_load(move |webview, payload| {
                use tauri::webview::PageLoadEvent;
                if payload.event() == PageLoadEvent::Finished {
                    {
                        let state = match state_clone.lock() {
                            Ok(s) => s,
                            Err(_) => return,
                        };
                        let (lock, cvar) = &*state.page_loaded;
                        if let Ok(mut loaded) = lock.lock() {
                            *loaded = true;
                        }
                        cvar.notify_all();
                    }

                    let state_clone2 = state_clone.clone();
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
                                };
                                if let Ok(mut state) = state_clone2.lock() {
                                    state.latest_snapshot = Some(snapshot);
                                    // 更新活跃标签
                                    let aid = state.active_tab_id.clone();
                                    if let Some(active_id) = aid {
                                        if let Some(tab) = state.tabs.iter_mut().find(|t| t.id == active_id) {
                                            tab.url = page_url.clone();
                                            if !title.is_empty() {
                                                tab.title = title.clone();
                                            }
                                        }
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
                }
            });

        let webview = window
            .add_child(builder, LogicalPosition::new(x, y), LogicalSize::new(w, h))
            .map_err(|e| format!("创建浏览器 WebView 失败：{e}"))?;

        if let Ok(mut state) = self.state.lock() {
            state.webview = Some(webview);
            // 创建首个标签
            let tab_id = scru128::new().to_string();
            state.tabs.push(BrowserTab {
                id: tab_id.clone(),
                url: url.to_string(),
                title: String::new(),
            });
            state.active_tab_id = Some(tab_id);
        }

        // 启动 URL 变化轮询线程（on_page_load 在子 WebView 后续导航中不触发）
        self.start_url_poll(app, url);

        Ok(())
    }

    pub fn close(&self) -> Result<(), String> {
        if let Ok(mut state) = self.state.lock() {
            state
                .poll_stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(webview) = state.webview.take() {
                let _ = webview.close();
            }
            let (lock, cvar) = &*state.page_loaded;
            if let Ok(mut loaded) = lock.lock() {
                *loaded = false;
            }
            cvar.notify_all();
            state.latest_snapshot = None;
            state.last_known_url.clear();
            state.tabs.clear();
            state.active_tab_id = None;
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

        std::thread::Builder::new()
            .name("browser-url-poll".into())
            .spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let current_url = {
                        let s = match state.lock() {
                            Ok(s) => s,
                            Err(e) => e.into_inner(),
                        };
                        match s.webview {
                            Some(ref wv) => match wv.url() {
                                Ok(u) => u.to_string(),
                                Err(_) => continue,
                            },
                            None => break,
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
                        // 更新活跃标签 URL（标题通过 on_page_load 回调更新）
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
                }
                debug!("browser url_poll thread exiting");
            })
            .expect("failed to spawn browser URL poll thread");
    }

    pub fn hide(&self) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(webview) = &state.webview {
            let _ = webview.set_size(LogicalSize::new(0.0, 0.0));
            let _ = webview.set_position(LogicalPosition::new(-10000, -10000));
        }
        Ok(())
    }

    pub fn go_back(&self) -> Result<(), String> {
        self.eval("history.back()")
    }

    pub fn go_forward(&self) -> Result<(), String> {
        self.eval("history.forward()")
    }

    pub fn set_position(&self, x: f64, y: f64) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(webview) = &state.webview {
            webview
                .set_position(LogicalPosition::new(x, y))
                .map_err(|e| format!("设置浏览器位置失败：{e}"))?;
        }
        Ok(())
    }

    pub fn set_size(&self, w: f64, h: f64) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(webview) = &state.webview {
            webview
                .set_size(LogicalSize::new(w, h))
                .map_err(|e| format!("设置浏览器尺寸失败：{e}"))?;
        }
        Ok(())
    }

    pub fn navigate(&self, url: &str) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        let (lock, _cvar) = &*state.page_loaded;
        if let Ok(mut loaded) = lock.lock() {
            *loaded = false;
        }
        if let Some(webview) = &state.webview {
            let parsed_url: Url = url.parse().map_err(|e| format!("URL 解析失败：{e}"))?;
            webview
                .navigate(parsed_url)
                .map_err(|e| format!("导航失败：{e}"))?;
        }
        Ok(())
    }

    pub fn load_html(&self, html: &str) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        let (lock, _cvar) = &*state.page_loaded;
        if let Ok(mut loaded) = lock.lock() {
            *loaded = false;
        }
        if let Some(webview) = &state.webview {
            let encoded = base64_url::encode(html.as_bytes());
            let data_url = format!("data:text/html;base64,{encoded}");
            let parsed_url: Url = data_url
                .parse()
                .map_err(|e| format!("data URL 构造失败：{e}"))?;
            webview
                .navigate(parsed_url)
                .map_err(|e| format!("加载 HTML 失败：{e}"))?;
        }
        Ok(())
    }

    pub fn eval(&self, js: &str) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(webview) = &state.webview {
            webview.eval(js).map_err(|e| format!("执行 JS 失败：{e}"))?;
        }
        Ok(())
    }

    pub(crate) fn eval_with_result(&self, js: &str) -> Option<String> {
        let (sender, rx) = std::sync::mpsc::channel();
        let tx = Arc::new(std::sync::Mutex::new(Some(sender)));
        {
            let state = self.state.lock().ok()?;
            let webview = state.webview.as_ref()?;
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
        rx.recv_timeout(Duration::from_secs(10)).ok()
    }

    fn wait_for_page_load(&self, timeout_ms: u64) -> bool {
        let page_loaded = {
            let state = match self.state.lock() {
                Ok(s) => s,
                Err(_) => return false,
            };
            state.page_loaded.clone()
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
        state.latest_snapshot.clone()
    }

    pub fn tab_list(&self) -> Vec<BrowserTab> {
        self.state
            .lock()
            .map(|s| s.tabs.clone())
            .unwrap_or_default()
    }

    pub fn tab_new(&self, url: &str) -> Result<String, String> {
        let tab_id = scru128::new().to_string();
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        state.tabs.push(BrowserTab {
            id: tab_id.clone(),
            url: url.to_string(),
            title: String::new(),
        });
        state.active_tab_id = Some(tab_id.clone());
        // 导航到新标签 URL
        if let Some(webview) = &state.webview {
            let parsed_url: Url = url.parse().map_err(|e| format!("URL 解析失败：{e}"))?;
            webview
                .navigate(parsed_url)
                .map_err(|e| format!("导航失败：{e}"))?;
        }
        Ok(tab_id)
    }

    pub fn tab_switch(&self, tab_id: &str) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        let tab_url = state
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .map(|t| t.url.clone())
            .ok_or_else(|| format!("标签 {tab_id} 不存在"))?;

        if state.active_tab_id.as_deref() == Some(tab_id) {
            return Ok(());
        }

        state.active_tab_id = Some(tab_id.to_string());
        if let Some(webview) = &state.webview {
            let parsed_url: Url = tab_url.parse().map_err(|e| format!("URL 解析失败：{e}"))?;
            webview
                .navigate(parsed_url)
                .map_err(|e| format!("导航失败：{e}"))?;
        }
        Ok(())
    }

    pub fn tab_close(&self, tab_id: &str) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        let pos = state
            .tabs
            .iter()
            .position(|t| t.id == tab_id)
            .ok_or_else(|| format!("标签 {tab_id} 不存在"))?;

        state.tabs.remove(pos);
        let was_active = state.active_tab_id.as_deref() == Some(tab_id);

        if was_active {
            if state.tabs.is_empty() {
                // 关闭最后一个标签时创建空标签
                let new_id = scru128::new().to_string();
                state.tabs.push(BrowserTab {
                    id: new_id.clone(),
                    url: "about:blank".to_string(),
                    title: String::new(),
                });
                state.active_tab_id = Some(new_id);
                if let Some(webview) = &state.webview {
                    let _ = webview.navigate(Url::parse("about:blank").unwrap());
                }
            } else {
                let new_pos = pos.min(state.tabs.len() - 1);
                let (new_id, new_url) = {
                    let t = &state.tabs[new_pos];
                    (t.id.clone(), t.url.clone())
                };
                state.active_tab_id = Some(new_id);
                if let Some(webview) = &state.webview {
                    if let Ok(parsed_url) = new_url.parse::<Url>() {
                        let _ = webview.navigate(parsed_url);
                    }
                }
            }
        }
        Ok(())
    }

    /// 更新活跃标签的 URL 和标题（页面加载/导航时调用）
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
