use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewBuilder,
    WebviewUrl, Wry,
};
use tokio::sync::oneshot;

const BROWSER_WEBVIEW_LABEL: &str = "browser-webview";

const BRIDGE_SCRIPT: &str = r#"
(function() {
    if (window.__tiangong_bridge_loaded) return;
    window.__tiangong_bridge_loaded = true;

    // 拦截 __TAURI__ 使外部页面无法访问 Tauri API（如 shell.open）
    // eval_with_callback 使用 __TAURI_INTERNALS__，不受影响
    // 使用属性拦截器而非简单判断，确保无论 Tauri 何时注入都能生效
    (function() {
        var _tauri;
        try {
            Object.defineProperty(window, '__TAURI__', {
                get: function() { return undefined; },
                set: function(val) { _tauri = val; },
                configurable: true
            });
        } catch(e) {}
    })();

    window.__tiangong_bridge = {
        version: '0.5.0',

        getFullText: function(maxChars) {
            maxChars = maxChars || 12000;
            var text = '';
            if (document.body) {
                var clone = document.body.cloneNode(true);
                var removes = clone.querySelectorAll('script,style,noscript');
                for (var i = 0; i < removes.length; i++) {
                    removes[i].parentNode.removeChild(removes[i]);
                }
                text = (clone.textContent || '').replace(/\s+/g, ' ').trim();
                if (text.length < 50) {
                    text = (document.body.innerText || '').trim();
                }
            }
            if (text.length > maxChars) {
                text = text.substring(0, maxChars) + '\n...[内容已截断]';
            }
            return {
                title: document.title,
                url: window.location.href,
                text: text,
            };
        },

        click: function(selector) {
            var el = document.querySelector(selector);
            if (el) { el.click(); return true; }
            return false;
        },

        type: function(selector, text) {
            var el = document.querySelector(selector);
            if (!el) return false;
            el.focus();
            var nativeSetter = Object.getOwnPropertyDescriptor(
                HTMLInputElement.prototype, 'value'
            );
            if (nativeSetter && nativeSetter.set) {
                nativeSetter.set.call(el, text);
            } else {
                el.value = text;
            }
            el.dispatchEvent(new Event('input', { bubbles: true }));
            el.dispatchEvent(new Event('change', { bubbles: true }));
            return true;
        },
    };

    console.log('[Tiangong Bridge] loaded v0.5.0');
})();
"#;

/// 浏览器 WebView 的共享状态
pub struct BrowserState {
    webview: Option<Webview<Wry>>,
    /// 页面加载完成信号
    page_loaded: Arc<(Mutex<bool>, Condvar)>,
    /// 最近一次页面快照
    latest_snapshot: Option<tiangong_types::BrowserPageSnapshot>,
}

pub struct BrowserManager {
    state: Arc<Mutex<BrowserState>>,
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
        self.close()?;

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
                    // 通知页面加载完成
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

                    // 自动捕获页面快照并通知前端
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
                                let snapshot = tiangong_types::BrowserPageSnapshot {
                                    title: title.clone(),
                                    url: page_url.clone(),
                                    text: text.clone(),
                                    status: tiangong_types::PageStatus::Loaded,
                                };
                                if let Ok(mut state) = state_clone2.lock() {
                                    state.latest_snapshot = Some(snapshot);
                                }
                                // 通知前端页面加载完成
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
        }

        Ok(())
    }

    pub fn close(&self) -> Result<(), String> {
        if let Ok(mut state) = self.state.lock() {
            if let Some(webview) = state.webview.take() {
                let _ = webview.close();
            }
            let (lock, cvar) = &*state.page_loaded;
            if let Ok(mut loaded) = lock.lock() {
                *loaded = false;
            }
            cvar.notify_all();
            state.latest_snapshot = None;
        }
        Ok(())
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

    pub fn eval(&self, js: &str) -> Result<(), String> {
        let state = self.state.lock().map_err(|e| e.to_string())?;
        if let Some(webview) = &state.webview {
            webview.eval(js).map_err(|e| format!("执行 JS 失败：{e}"))?;
        }
        Ok(())
    }

    /// 通过 eval_with_callback 获取 JS 返回值
    fn eval_with_result(&self, js: &str) -> Option<String> {
        let (sender, rx) = oneshot::channel();
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
        rx.blocking_recv().ok()
    }

    /// 等待页面加载完成
    ///
    /// 使用循环处理 close() 的过期 notify 和虚假唤醒，
    /// 直到 loaded=true 或超时。
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
            let result = cvar.wait_timeout(guard, std::time::Duration::from_millis(remaining));
            match result {
                Ok((g, _)) => {
                    guard = g;
                    if *guard {
                        return true;
                    }
                    // 过期通知或虚假唤醒，继续等待
                }
                Err(_) => return false,
            }
        }
    }

    /// 轮询等待页面内容渲染就绪（处理 JS 异步渲染）
    ///
    /// 策略：等待内容增长后稳定，而非仅检查阈值。
    /// - 内容 > 1000 字符且连续 2 次不变 → 快速路径（静态页面已加载）
    /// - 内容曾增长且连续 3 次不变 → 慢速路径（JS 渲染完成）
    /// - 否则持续轮询直到超时
    fn wait_for_content_ready(&self, timeout_ms: u64) {
        let start = std::time::Instant::now();
        let check_interval = std::time::Duration::from_millis(500);
        let timeout = std::time::Duration::from_millis(timeout_ms);
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

    /// 通过浏览器获取网页内容
    pub fn fetch_page_content(
        &self,
        url: &str,
        max_chars: usize,
        should_navigate: bool,
    ) -> tiangong_types::BrowserResponse {
        let error_response = |err: String| tiangong_types::BrowserResponse {
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
        eprintln!(
            "[browser] wait_for_page_load={loaded}, elapsed={}ms",
            t0.elapsed().as_millis()
        );

        let t1 = std::time::Instant::now();
        self.wait_for_content_ready(15_000);
        eprintln!(
            "[browser] wait_for_content_ready elapsed={}ms",
            t1.elapsed().as_millis()
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
                    tiangong_types::BrowserResponse {
                        ok: true,
                        title,
                        content,
                        final_url,
                        error: None,
                    }
                }
                Err(e) => {
                    eprintln!("[browser] JSON parse error: {e}");
                    error_response("解析页面内容失败".to_string())
                }
            },
            None => error_response("获取页面内容超时".to_string()),
        }
    }

    /// 获取当前浏览器页面的快照
    pub fn get_snapshot(&self) -> Option<tiangong_types::BrowserPageSnapshot> {
        let state = self.state.lock().ok()?;
        state.latest_snapshot.clone()
    }
}

fn browser_data_directory() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".tiangong").join("browser-data")
}

/// 浏览器命令处理循环（异步任务）
pub async fn browser_command_handler(
    mut rx: tokio::sync::mpsc::Receiver<tiangong_types::BrowserCommand>,
    browser_state: Arc<Mutex<BrowserState>>,
    app: AppHandle<Wry>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            tiangong_types::BrowserCommand::FetchPage {
                url,
                max_chars,
                response_tx,
            } => {
                let url_for_error = url.clone();
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };

                let should_navigate = if !manager.is_open() {
                    if let Some((x, y, w, h)) = default_browser_rect(&app) {
                        let _ = manager.open(&app, &url, x, y, w, h);
                    }
                    let _ = app.emit("browser:open", &url);
                    false
                } else {
                    true
                };

                let result = tokio::task::spawn_blocking(move || {
                    manager.fetch_page_content(&url, max_chars, should_navigate)
                })
                .await;
                let response = result.unwrap_or(tiangong_types::BrowserResponse {
                    ok: false,
                    title: String::new(),
                    content: String::new(),
                    final_url: url_for_error,
                    error: Some("浏览器任务执行失败".to_string()),
                });
                let _ = response_tx.send(response);
            }
            tiangong_types::BrowserCommand::OpenUrl { url } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                if !manager.is_open() {
                    if let Some((x, y, w, h)) = default_browser_rect(&app) {
                        let _ = manager.open(&app, &url, x, y, w, h);
                    }
                    let _ = app.emit("browser:open", &url);
                } else {
                    let _ = manager.navigate(&url);
                }
            }
            tiangong_types::BrowserCommand::ObservePage { response_tx } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let snapshot = tokio::task::spawn_blocking(move || {
                    manager
                        .eval_with_result("window.__tiangong_bridge.getFullText(12000)")
                        .and_then(|raw| {
                            let data = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
                            Some(tiangong_types::BrowserPageSnapshot {
                                title: data["title"].as_str().unwrap_or("").to_string(),
                                url: data["url"].as_str().unwrap_or("").to_string(),
                                text: data["text"].as_str().unwrap_or("").to_string(),
                                status: tiangong_types::PageStatus::Loaded,
                            })
                        })
                        .unwrap_or(tiangong_types::BrowserPageSnapshot {
                            title: String::new(),
                            url: String::new(),
                            text: String::new(),
                            status: tiangong_types::PageStatus::Error(
                                "浏览器未打开或页面未加载".to_string(),
                            ),
                        })
                })
                .await
                .unwrap_or(tiangong_types::BrowserPageSnapshot {
                    title: String::new(),
                    url: String::new(),
                    text: String::new(),
                    status: tiangong_types::PageStatus::Error("浏览器快照任务失败".to_string()),
                });
                let _ = response_tx.send(snapshot);
            }
        }
    }
}

fn default_browser_rect(app: &AppHandle<Wry>) -> Option<(f64, f64, f64, f64)> {
    let window = app.get_window("main")?;
    let size = window.inner_size().ok()?;
    let scale = window.scale_factor().unwrap_or(1.0);
    let w = size.width as f64 / scale;
    let h = size.height as f64 / scale;
    let browser_w = w * 0.5;
    Some((w - browser_w, 0.0, browser_w, h))
}
