use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Wry};
use tokio::sync::mpsc;

use crate::manager::{default_browser_rect, BrowserManager, BrowserState};
use crate::types::{BrowserCommand, BrowserPageSnapshot, BrowserResponse, PageStatus};

/// 浏览器命令处理循环
pub async fn browser_command_handler(
    mut rx: mpsc::Receiver<BrowserCommand>,
    browser_state: Arc<Mutex<BrowserState>>,
    app: AppHandle<Wry>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            BrowserCommand::FetchPage {
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
                let response = result.unwrap_or(BrowserResponse {
                    ok: false,
                    title: String::new(),
                    content: String::new(),
                    final_url: url_for_error,
                    error: Some("浏览器任务执行失败".to_string()),
                });
                let _ = response_tx.send(response);
            }
            BrowserCommand::OpenUrl { url } => {
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
            BrowserCommand::ObservePage { response_tx } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let snapshot = tokio::task::spawn_blocking(move || {
                    manager
                        .eval_with_result("window.__tiangong_bridge.getFullText(12000)")
                        .and_then(|raw| {
                            let data = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
                            Some(BrowserPageSnapshot {
                                title: data["title"].as_str().unwrap_or("").to_string(),
                                url: data["url"].as_str().unwrap_or("").to_string(),
                                text: data["text"].as_str().unwrap_or("").to_string(),
                                status: PageStatus::Loaded,
                            })
                        })
                        .unwrap_or(BrowserPageSnapshot {
                            title: String::new(),
                            url: String::new(),
                            text: String::new(),
                            status: PageStatus::Error("浏览器未打开或页面未加载".to_string()),
                        })
                })
                .await
                .unwrap_or(BrowserPageSnapshot {
                    title: String::new(),
                    url: String::new(),
                    text: String::new(),
                    status: PageStatus::Error("浏览器快照任务失败".to_string()),
                });
                let _ = response_tx.send(snapshot);
            }
        }
    }
}
