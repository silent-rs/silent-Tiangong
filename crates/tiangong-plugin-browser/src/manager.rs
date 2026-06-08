use tracing::{debug, warn};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewBuilder, WebviewUrl, Wry,
};
use tokio::sync::mpsc;

use crate::bridge::BRIDGE_SCRIPT;
use crate::types::{BrowserEvent, BrowserPageSnapshot, BrowserResponse, BrowserTab};

const BROWSER_WEBVIEW_LABEL: &str = "browser-webview";

/// 浏览器 WebView 的共享状态
pub struct BrowserState {
    pub webview: Option<Webview<Wry>>,
    /// 最近一次页面快照
    pub latest_snapshot: Option<BrowserPageSnapshot>,
    /// 浏览器监测任务停止信号
    pub watcher_stop: Arc<AtomicBool>,
    /// 事件发送端（用于 watcher 等即时通知）
    pub event_tx: Option<mpsc::Sender<BrowserEvent>>,
    /// 标签列表
    pub tabs: Vec<BrowserTab>,
    /// 活跃标签 ID
    pub active_tab_id: Option<String>,
    /// web_fetch 同步获取内容时设为 true，阻止 watcher 推送重复内容
    pub sync_fetch_in_progress: Arc<AtomicBool>,
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
                latest_snapshot: None,
                watcher_stop: Arc::new(AtomicBool::new(false)),
                event_tx: None,
                tabs: Vec::new(),
                active_tab_id: None,
                sync_fetch_in_progress: Arc::new(AtomicBool::new(false)),
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

        // 注意：不使用 .on_page_load()，因为 wry 内部的 url_from_webview
        // 会在 WebView URL 为 None 时 panic（wry bug）。
        // 页面加载状态改由 wait_for_page_ready 通过 JS 轮询检测。
        let builder = WebviewBuilder::new(BROWSER_WEBVIEW_LABEL, WebviewUrl::External(parsed_url))
            .initialization_script(BRIDGE_SCRIPT)
            .data_directory(data_dir)
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/605.1.15")
            .enable_clipboard_access()
            .devtools(true);

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

        Ok(())
    }

    pub fn close(&self) -> Result<(), String> {
        if let Ok(mut state) = self.state.lock() {
            if let Some(webview) = state.webview.take() {
                let _ = webview.close();
            }
            state.latest_snapshot = None;
            state.tabs.clear();
            state.active_tab_id = None;
        }
        Ok(())
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
        if let Some(webview) = &state.webview {
            let parsed_url: Url = url.parse().map_err(|e| format!("URL 解析失败：{e}"))?;
            // wry 的 navigate 内部 NSURL::URLWithString 可能 panic，用 catch_unwind 保护
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                webview.navigate(parsed_url)
            }));
            result
                .map_err(|_| "WebView 导航内部错误".to_string())?
                .map_err(|e| format!("导航失败：{e}"))?;
        }
        Ok(())
    }

    pub fn load_html(&self, html: &str) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(webview) = &state.webview {
            let encoded = base64_url::encode(html.as_bytes());
            let data_url = format!("data:text/html;base64,{encoded}");
            let parsed_url: Url = data_url
                .parse()
                .map_err(|e| format!("data URL 构造失败：{e}"))?;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                webview.navigate(parsed_url)
            }));
            result
                .map_err(|_| "WebView 导航内部错误".to_string())?
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

    fn reset_sync_fetch_flag(&self) {
        if let Ok(state) = self.state.lock() {
            state.sync_fetch_in_progress.store(false, Ordering::Relaxed);
        }
    }

    /// 通过 JS 轮询等待页面加载完成（document.readyState === 'complete'）。
    pub(crate) fn wait_for_page_ready(&self, timeout_ms: u64) -> bool {
        let start = std::time::Instant::now();
        let check_interval = Duration::from_millis(300);

        loop {
            if let Some(result) = self.eval_with_result("document.readyState") {
                if result.contains("complete") || result.contains("interactive") {
                    return true;
                }
            }

            if start.elapsed() >= Duration::from_millis(timeout_ms) {
                return false;
            }
            std::thread::sleep(check_interval);
        }
    }

    /// 等待页面内容稳定：连续两次采集到完全相同的文本内容视为就绪。
    pub(crate) fn wait_for_content_ready(&self, timeout_ms: u64) {
        let start = std::time::Instant::now();
        let check_interval = Duration::from_millis(500);
        let mut prev_text = String::new();
        let mut stable_count: usize = 0;

        loop {
            if let Some(raw) =
                self.eval_with_result("JSON.stringify(window.__tiangong_bridge.getFullText(12000))")
            {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                    let text = data["text"].as_str().unwrap_or("").to_string();
                    if !text.is_empty() && text == prev_text {
                        stable_count += 1;
                        if stable_count >= 2 {
                            break;
                        }
                    } else {
                        stable_count = 0;
                    }
                    prev_text = text;
                }
            }

            if start.elapsed() >= Duration::from_millis(timeout_ms) {
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

        // 标记同步获取进行中，阻止 watcher 推送重复内容
        {
            let state = match self.state.lock() {
                Ok(s) => s,
                Err(e) => e.into_inner(),
            };
            state.sync_fetch_in_progress.store(true, Ordering::Relaxed);
        }

        if should_navigate {
            if let Err(err) = self.navigate(url) {
                self.reset_sync_fetch_flag();
                return error_response(err);
            }
        }

        let t0 = std::time::Instant::now();
        let ready = self.wait_for_page_ready(15_000);
        debug!(
            ready,
            elapsed_ms = t0.elapsed().as_millis() as u64,
            "browser wait_for_page_ready"
        );

        if !ready {
            self.reset_sync_fetch_flag();
            return error_response("页面加载超时".to_string());
        }

        let t1 = std::time::Instant::now();
        self.wait_for_content_ready(15_000);
        debug!(
            elapsed_ms = t1.elapsed().as_millis() as u64,
            "browser wait_for_content_ready"
        );

        let result = self.eval_with_result(&format!(
            "JSON.stringify(window.__tiangong_bridge.getFullText({max_chars}))"
        ));

        self.reset_sync_fetch_flag();

        match result {
            Some(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(data) => {
                    let title = data["title"].as_str().unwrap_or("").to_string();
                    let content = data["text"].as_str().unwrap_or("").to_string();
                    let final_url = data["url"].as_str().unwrap_or(url).to_string();

                    // 同步更新快照和活跃标签
                    if let Ok(mut state) = self.state.lock() {
                        state.latest_snapshot = Some(BrowserPageSnapshot {
                            title: title.clone(),
                            url: final_url.clone(),
                            text: content.clone(),
                            status: crate::types::PageStatus::Loaded,
                            tabs: Vec::new(),
                            active_tab_id: None,
                        });
                        let aid = state.active_tab_id.clone();
                        if let Some(active_id) = aid {
                            if let Some(tab) = state.tabs.iter_mut().find(|t| t.id == active_id) {
                                tab.url = final_url.clone();
                                if !title.is_empty() {
                                    tab.title = title.clone();
                                }
                            }
                        }
                    }

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
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = webview.navigate(parsed_url);
            }));
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
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = webview.navigate(parsed_url);
            }));
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
                    if let Ok(parsed_url) = Url::parse("about:blank") {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _ = webview.navigate(parsed_url);
                        }));
                    }
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
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _ = webview.navigate(parsed_url);
                        }));
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
