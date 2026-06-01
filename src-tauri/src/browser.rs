use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewBuilder,
    WebviewUrl, Wry,
};
use tokio::sync::oneshot;

const BROWSER_WEBVIEW_LABEL: &str = "browser-webview";
const RESULT_PREFIX: &str = "__TG_RESULT__";

const BRIDGE_SCRIPT: &str = r#"
(function() {
    if (window.__tiangong_bridge_loaded) return;
    window.__tiangong_bridge_loaded = true;

    window.__tiangong_bridge = {
        version: '0.2.0',

        getPageInfo: function() {
            return JSON.stringify({
                title: document.title,
                url: window.location.href,
                ready: document.readyState === 'complete' || document.readyState === 'interactive',
            });
        },

        getFullText: function(maxChars) {
            maxChars = maxChars || 12000;
            var text = '';
            if (document.body) {
                text = document.body.innerText || '';
            }
            if (text.length > maxChars) {
                text = text.substring(0, maxChars) + '\n...[内容已截断]';
            }
            return JSON.stringify({
                title: document.title,
                url: window.location.href,
                text: text,
            });
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

        // 通过 title 变更将结果传递给 Rust
        returnResult: function(data) {
            document.title = '__TG_RESULT__' + data;
        },
    };

    console.log('[Tiangong Bridge] loaded v0.2.0');
})();
"#;

/// 浏览器 WebView 的共享状态
pub struct BrowserState {
    webview: Option<Webview<Wry>>,
    /// 等待中的 title 变更结果接收器
    pending_result: Option<oneshot::Sender<String>>,
    /// 原始页面标题（用于恢复）
    original_title: String,
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
                pending_result: None,
                original_title: String::new(),
            })),
        }
    }

    /// 克隆内部状态引用（用于异步任务）
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

        let builder = WebviewBuilder::new(BROWSER_WEBVIEW_LABEL, WebviewUrl::External(parsed_url))
            .initialization_script(BRIDGE_SCRIPT)
            .data_directory(data_dir)
            .enable_clipboard_access()
            .devtools(true)
            .on_document_title_changed(move |_webview, title| {
                if let Some(data) = title.strip_prefix(RESULT_PREFIX) {
                    let mut state = match state_clone.lock() {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    if let Some(tx) = state.pending_result.take() {
                        let _ = tx.send(data.to_string());
                    }
                } else if let Ok(mut state) = state_clone.lock() {
                    state.original_title = title.to_string();
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
            state.pending_result = None;
            state.original_title = String::new();
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

    /// 通过浏览器获取网页内容
    ///
    /// 流程：导航到 URL → 等待加载 → 通过 title IPC 提取页面文本
    pub fn fetch_page_content(
        &self,
        url: &str,
        max_chars: usize,
    ) -> tiangong_types::BrowserResponse {
        // 导航到目标 URL
        if let Err(err) = self.navigate(url) {
            return tiangong_types::BrowserResponse {
                ok: false,
                title: String::new(),
                content: String::new(),
                final_url: url.to_string(),
                error: Some(err),
            };
        }

        // 等待页面加载
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }

        // 提取页面内容（通过 title IPC）
        let result = self.eval_with_result(&format!(
            "window.__tiangong_bridge.returnResult(window.__tiangong_bridge.getFullText({max_chars}))",
        ));

        match result {
            Some(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(data) => tiangong_types::BrowserResponse {
                    ok: true,
                    title: data["title"].as_str().unwrap_or("").to_string(),
                    content: data["text"].as_str().unwrap_or("").to_string(),
                    final_url: data["url"].as_str().unwrap_or(url).to_string(),
                    error: None,
                },
                Err(_) => tiangong_types::BrowserResponse {
                    ok: false,
                    title: String::new(),
                    content: String::new(),
                    final_url: url.to_string(),
                    error: Some("解析页面内容失败".to_string()),
                },
            },
            None => tiangong_types::BrowserResponse {
                ok: false,
                title: String::new(),
                content: String::new(),
                final_url: url.to_string(),
                error: Some("获取页面内容超时".to_string()),
            },
        }
    }

    /// 执行 JS 并通过 title 变更获取返回值
    fn eval_with_result(&self, js: &str) -> Option<String> {
        let (tx, rx) = oneshot::channel();

        // 注册等待器
        {
            let mut state = self.state.lock().ok()?;
            state.pending_result = Some(tx);
        }

        // 执行 JS
        self.eval(js).ok()?;

        // 等待结果（最多 10 秒）
        rx.blocking_recv().ok()
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

                // 浏览器未打开时自动创建并通知前端
                if !manager.is_open() {
                    if let Some((x, y, w, h)) = default_browser_rect(&app) {
                        let _ = manager.open(&app, &url, x, y, w, h);
                    }
                    let _ = app.emit("browser:open", &url);
                    // 等待浏览器创建完成
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }

                let result = tokio::task::spawn_blocking(move || {
                    manager.fetch_page_content(&url, max_chars)
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
        }
    }
}

/// 根据主窗口尺寸计算浏览器面板默认位置（右侧 50%）
fn default_browser_rect(app: &AppHandle<Wry>) -> Option<(f64, f64, f64, f64)> {
    let window = app.get_window("main")?;
    let size = window.inner_size().ok()?;
    let scale = window.scale_factor().unwrap_or(1.0);
    let w = size.width as f64 / scale;
    let h = size.height as f64 / scale;
    let browser_w = w * 0.5;
    Some((w - browser_w, 0.0, browser_w, h))
}
