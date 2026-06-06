use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Wry};
use tokio::sync::mpsc;
use tracing::warn;

use crate::manager::{default_browser_rect, BrowserManager, BrowserState};
use crate::types::{
    AnnotationExtractResult, BrowserCommand, BrowserPageSnapshot, BrowserResponse,
    ClickElementResult, FillFieldResult, FormExtractResult, PageStatus,
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
                // 浏览器未打开时不返回响应，让 observe_page() 返回 None
                {
                    let s = match browser_state.lock() {
                        Ok(s) => s,
                        Err(e) => e.into_inner(),
                    };
                    if s.webview.is_none() {
                        continue;
                    }
                }
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let snapshot = tokio::task::spawn_blocking(move || {
                    manager
                        .eval_with_result(
                            "(function(){try{var t=window.__tiangong_bridge.getFullText(12000);var a=window.__tiangong_bridge.annotation.getAnnotations();if(a&&a.count>0){t.text+='\\n\\n[页面批注] '+JSON.stringify(a.annotations);}return t;}catch(e){return {title:'',url:'',text:'',error:e.message};}})()"
                        )
                        .and_then(|raw| {
                            let data = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
                            Some(BrowserPageSnapshot {
                                title: data["title"].as_str().unwrap_or("").to_string(),
                                url: data["url"].as_str().unwrap_or("").to_string(),
                                text: data["text"].as_str().unwrap_or("").to_string(),
                                status: PageStatus::Loaded,
                                tabs: Vec::new(),
                                active_tab_id: None,
                            })
                        })
                        .unwrap_or(BrowserPageSnapshot {
                            title: String::new(),
                            url: String::new(),
                            text: String::new(),
                            status: PageStatus::Error("浏览器未打开或页面未加载".to_string()),
                            tabs: Vec::new(),
                            active_tab_id: None,
                        })
                })
                .await
                .unwrap_or(BrowserPageSnapshot {
                    title: String::new(),
                    url: String::new(),
                    text: String::new(),
                    status: PageStatus::Error("浏览器快照任务失败".to_string()),
                    tabs: Vec::new(),
                    active_tab_id: None,
                });
                // 补充标签信息
                let tabs = {
                    let s = match browser_state.lock() {
                        Ok(s) => s,
                        Err(e) => e.into_inner(),
                    };
                    let active_tab_id = s.active_tab_id.clone();
                    let tabs = s.tabs.clone();
                    (tabs, active_tab_id)
                };
                let snapshot = BrowserPageSnapshot {
                    tabs: tabs.0,
                    active_tab_id: tabs.1,
                    ..snapshot
                };
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
                wait_for,
                response_tx,
            } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let result = tokio::task::spawn_blocking(move || {
                    // 操作前 digest
                    let before_digest = manager
                        .eval_with_result("JSON.stringify(window.__tiangong_bridge.getPageDigest())");

                    // 先尝试原生 fillField
                    let js = format!(
                        "window.__tiangong_bridge.fillField({},{},{})",
                        serde_json::to_string(&selector).unwrap_or_default(),
                        serde_json::to_string(&value).unwrap_or_default(),
                        serde_json::to_string(&strategy).unwrap_or_default(),
                    );
                    let mut native_result = manager
                        .eval_with_result(&js)
                        .and_then(|raw| serde_json::from_str::<FillFieldResult>(&raw).ok())
                        .unwrap_or(FillFieldResult {
                            ok: false,
                            strategy: None,
                            error: Some("填写字段执行失败".to_string()),
                            current_value: None,
                            wait_result: None,
                            page_diff: None,
                        });

                    if !native_result.ok {
                        // 原生策略失败，尝试 UI 库组件填写
                        let comp_js = format!(
                            "window.__tiangong_bridge.fillComponent({},{})",
                            serde_json::to_string(&selector).unwrap_or_default(),
                            serde_json::to_string(&value).unwrap_or_default(),
                        );
                        native_result = manager
                            .eval_with_result(&comp_js)
                            .and_then(|raw| serde_json::from_str::<FillFieldResult>(&raw).ok())
                            .unwrap_or(native_result);
                    }

                    // 填写成功后执行等待
                    if native_result.ok {
                        if let Some(ref condition) = wait_for {
                            let wait_js = format!(
                                "(async function(){{return JSON.stringify(await window.__tiangong_bridge.waitFor({},5000))}})()",
                                serde_json::to_string(condition).unwrap_or_default(),
                            );
                            if let Some(wait_raw) = manager.eval_with_result(&wait_js) {
                                native_result.wait_result =
                                    serde_json::from_str(&wait_raw).ok();
                            }
                        }

                        // 操作后 digest 对比
                        let after_digest = manager
                            .eval_with_result("JSON.stringify(window.__tiangong_bridge.getPageDigest())");
                        if let (Some(before), Some(after)) = (before_digest, after_digest) {
                            let diff_js = format!(
                                "JSON.stringify(window.__tiangong_bridge.diffDigest({},{}))",
                                before, after
                            );
                            if let Some(diff_raw) = manager.eval_with_result(&diff_js) {
                                let diff = diff_raw.trim_matches('"').replace("\\n", "\n");
                                if !diff.is_empty() {
                                    native_result.page_diff = Some(diff);
                                }
                            }
                        }
                    }

                    native_result
                })
                .await
                .unwrap_or(FillFieldResult {
                    ok: false,
                    strategy: None,
                    error: Some("填写字段任务失败".to_string()),
                    current_value: None,
                    wait_result: None,
                    page_diff: None,
                });
                let _ = response_tx.send(result);
            }
            BrowserCommand::ClickElement {
                selector,
                wait_for,
                response_tx,
            } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let result = tokio::task::spawn_blocking(move || {
                    // 操作前 digest
                    let before_digest = manager
                        .eval_with_result("JSON.stringify(window.__tiangong_bridge.getPageDigest())");

                    let js = format!(
                        "window.__tiangong_bridge.clickElement({})",
                        serde_json::to_string(&selector).unwrap_or_default(),
                    );
                    let mut result = manager
                        .eval_with_result(&js)
                        .and_then(|raw| serde_json::from_str::<ClickElementResult>(&raw).ok())
                        .unwrap_or(ClickElementResult {
                            ok: false,
                            error: Some("点击元素执行失败".to_string()),
                            wait_result: None,
                            candidates: vec![],
                            page_diff: None,
                        });

                    // 点击成功后执行等待
                    if result.ok {
                        if let Some(ref condition) = wait_for {
                            let wait_js = format!(
                                "(async function(){{return JSON.stringify(await window.__tiangong_bridge.waitFor({},5000))}})()",
                                serde_json::to_string(condition).unwrap_or_default(),
                            );
                            if let Some(wait_raw) = manager.eval_with_result(&wait_js) {
                                result.wait_result = serde_json::from_str(&wait_raw).ok();
                            }
                        }

                        // 操作后 digest 对比
                        let after_digest = manager
                            .eval_with_result("JSON.stringify(window.__tiangong_bridge.getPageDigest())");
                        if let (Some(before), Some(after)) = (before_digest, after_digest) {
                            let diff_js = format!(
                                "JSON.stringify(window.__tiangong_bridge.diffDigest({},{}))",
                                before, after
                            );
                            if let Some(diff_raw) = manager.eval_with_result(&diff_js) {
                                let diff = diff_raw.trim_matches('"').replace("\\n", "\n");
                                if !diff.is_empty() {
                                    result.page_diff = Some(diff);
                                }
                            }
                        }
                    }

                    result
                })
                .await
                .unwrap_or(ClickElementResult {
                    ok: false,
                    error: Some("点击元素任务失败".to_string()),
                    wait_result: None,
                    candidates: vec![],
                    page_diff: None,
                });
                let _ = response_tx.send(result);
            }
            BrowserCommand::LoadHtml { html, response_tx } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                // 浏览器未打开时先打开，再加载 HTML
                if !manager.is_open() {
                    if let Some((x, y, w, h)) = default_browser_rect(&app) {
                        let _ = manager.open(&app, "about:blank", x, y, w, h);
                    }
                    let _ = app.emit("browser:open", "about:blank");
                }
                let result = tokio::task::spawn_blocking(move || manager.load_html(&html))
                    .await
                    .unwrap_or(Err("加载 HTML 任务失败".to_string()));
                let _ = response_tx.send(result);
            }
            BrowserCommand::TabList { response_tx } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let tabs = manager.tab_list();
                let _ = response_tx.send(tabs);
            }
            BrowserCommand::TabNew { url } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let app_clone = app.clone();
                let _ = tokio::task::spawn_blocking(move || match manager.tab_new(&url) {
                    Ok(tab_id) => {
                        let _ = app_clone.emit(
                            "browser:tab_updated",
                            serde_json::json!({ "action": "new", "tab_id": tab_id, "url": url }),
                        );
                    }
                    Err(e) => warn!(error = %e, "tab_new error"),
                })
                .await;
            }
            BrowserCommand::TabSwitch { tab_id } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let app_clone = app.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = manager.tab_switch(&tab_id) {
                        warn!(error = %e, "tab_switch error");
                    } else {
                        let _ = app_clone.emit(
                            "browser:tab_updated",
                            serde_json::json!({ "action": "switch", "tab_id": tab_id }),
                        );
                    }
                })
                .await;
            }
            BrowserCommand::TabClose { tab_id } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let app_clone = app.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = manager.tab_close(&tab_id) {
                        warn!(error = %e, "tab_close error");
                    } else {
                        let _ = app_clone.emit(
                            "browser:tab_updated",
                            serde_json::json!({ "action": "close", "tab_id": tab_id }),
                        );
                    }
                })
                .await;
            }
            BrowserCommand::AnnotationExtract { response_tx } => {
                let manager = BrowserManager {
                    state: browser_state.clone(),
                };
                let result = tokio::task::spawn_blocking(move || {
                    manager
                        .eval_with_result(
                            "window.__tiangong_bridge.annotation.extractAnnotatedElements()",
                        )
                        .and_then(|raw| serde_json::from_str::<AnnotationExtractResult>(&raw).ok())
                        .unwrap_or(AnnotationExtractResult {
                            elements: vec![],
                            count: 0,
                        })
                })
                .await
                .unwrap_or(AnnotationExtractResult {
                    elements: vec![],
                    count: 0,
                });
                let _ = response_tx.send(result);
            }
        }
    }
}
