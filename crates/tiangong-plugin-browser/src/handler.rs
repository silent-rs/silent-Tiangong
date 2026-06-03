use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Wry};
use tokio::sync::mpsc;

use crate::manager::{default_browser_rect, BrowserManager, BrowserState};
use crate::types::{
    BrowserCommand, BrowserPageSnapshot, BrowserResponse, ClickElementResult, FillFieldResult,
    FormExtractResult, PageStatus,
};

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
            BrowserCommand::FormExtract { response_tx } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let result = tokio::task::spawn_blocking(move || {
                    manager
                        .eval_with_result("window.__tiangong_bridge.extractForms()")
                        .and_then(|raw| serde_json::from_str::<FormExtractResult>(&raw).ok())
                        .unwrap_or(FormExtractResult { forms: vec![] })
                })
                .await
                .unwrap_or(FormExtractResult { forms: vec![] });
                let _ = response_tx.send(result);
            }
            BrowserCommand::FormFill {
                selector,
                value,
                strategy,
                response_tx,
            } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let result = tokio::task::spawn_blocking(move || {
                    let js = format!(
                        "window.__tiangong_bridge.fillField({},{},{})",
                        serde_json::to_string(&selector).unwrap_or_default(),
                        serde_json::to_string(&value).unwrap_or_default(),
                        serde_json::to_string(&strategy).unwrap_or_default(),
                    );
                    manager
                        .eval_with_result(&js)
                        .and_then(|raw| serde_json::from_str::<FillFieldResult>(&raw).ok())
                        .unwrap_or(FillFieldResult {
                            ok: false,
                            strategy: None,
                            error: Some("填写字段执行失败".to_string()),
                            current_value: None,
                        })
                })
                .await
                .unwrap_or(FillFieldResult {
                    ok: false,
                    strategy: None,
                    error: Some("填写字段任务失败".to_string()),
                    current_value: None,
                });
                let _ = response_tx.send(result);
            }
            BrowserCommand::ClickElement {
                selector,
                response_tx,
            } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let result = tokio::task::spawn_blocking(move || {
                    let js = format!(
                        "window.__tiangong_bridge.clickElement({})",
                        serde_json::to_string(&selector).unwrap_or_default(),
                    );
                    manager
                        .eval_with_result(&js)
                        .and_then(|raw| serde_json::from_str::<ClickElementResult>(&raw).ok())
                        .unwrap_or(ClickElementResult {
                            ok: false,
                            error: Some("点击元素执行失败".to_string()),
                        })
                })
                .await
                .unwrap_or(ClickElementResult {
                    ok: false,
                    error: Some("点击元素任务失败".to_string()),
                });
                let _ = response_tx.send(result);
            }
            BrowserCommand::LoadHtml { html, response_tx } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let result = tokio::task::spawn_blocking(move || manager.load_html(&html))
                    .await
                    .unwrap_or(Err("加载 HTML 任务失败".to_string()));
                let _ = response_tx.send(result);
            }
        }
    }
}
