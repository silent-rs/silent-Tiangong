use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::debug;

use crate::manager::{BrowserManager, BrowserState};
use crate::types::BrowserEvent;

/// 浏览器监测异步任务。
///
/// 随插件启动常驻，浏览器未打开时跳过检测。
/// 检测 URL 变化后：
/// 1. 更新活跃标签 URL
/// 2. 发送轻量级事件通知前端
/// 3. 当 sync_fetch_in_progress 为 false 时，启动内容采集并推送 PageData 事件
pub async fn run_browser_watcher(
    state: Arc<Mutex<BrowserState>>,
    event_tx: mpsc::Sender<BrowserEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    let mut last_url = String::new();

    loop {
        interval.tick().await;

        if stop.load(Ordering::Relaxed) {
            break;
        }

        // 读取当前 URL
        let current_url = {
            let s = match state.lock() {
                Ok(s) => s,
                Err(e) => e.into_inner(),
            };
            match s.webview {
                Some(ref wv) => {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wv.url())) {
                        Ok(Ok(u)) => u.to_string(),
                        _ => continue,
                    }
                }
                None => continue,
            }
        };

        // 检测 URL 变化（忽略 about:blank 等导航中间状态）
        if current_url == last_url || current_url == "about:blank" {
            continue;
        }
        last_url = current_url.clone();

        debug!(url = %current_url, "browser watcher detected URL change");

        // 更新活跃标签 URL
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

        // 检查是否有同步获取正在进行
        let sync_active = {
            let s = match state.lock() {
                Ok(s) => s,
                Err(e) => e.into_inner(),
            };
            s.sync_fetch_in_progress.load(Ordering::Relaxed)
        };

        if sync_active {
            // 同步获取进行中，只发送轻量级事件
            let _ = event_tx.try_send(BrowserEvent::PageData {
                url: current_url.clone(),

                title: String::new(),
                text: String::new(),
            });
            continue;
        }

        // 先发送轻量级事件让前端立即更新地址栏，不等待内容采集
        let _ = event_tx.try_send(BrowserEvent::PageData {
            url: current_url,
            title: String::new(),
            text: String::new(),
        });

        // 被动浏览（非 web_fetch 触发的 URL 变化）：采集内容并推送
        let state_clone = state.clone();
        let event_tx_clone = event_tx.clone();
        tokio::task::spawn_blocking(move || {
            let manager = BrowserManager { state: state_clone };

            // 等待页面加载
            if !manager.wait_for_page_ready(10_000) {
                return;
            }

            // 等待内容稳定
            manager.wait_for_content_ready(10_000);

            let js = "JSON.stringify(window.__tiangong_bridge.getFullText(12000))";
            let raw = match manager.eval_with_result(js) {
                Some(r) => r,
                None => return,
            };
            let data = match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(d) => d,
                Err(_) => return,
            };
            let title = data["title"].as_str().unwrap_or("").to_string();
            let url = data["url"].as_str().unwrap_or("").to_string();
            let text = data["text"].as_str().unwrap_or("").to_string();

            if url.is_empty() {
                return;
            }

            let summary: String = text.chars().take(2000).collect();

            // 更新快照
            {
                let mut s = match manager.state.lock() {
                    Ok(s) => s,
                    Err(e) => e.into_inner(),
                };
                s.latest_snapshot = Some(crate::types::BrowserPageSnapshot {
                    title: title.clone(),
                    url: url.clone(),
                    text: text.clone(),
                    status: crate::types::PageStatus::Loaded,
                    tabs: Vec::new(),
                    active_tab_id: None,
                });
                let aid = s.active_tab_id.clone();
                if let Some(active_id) = aid {
                    if let Some(tab) = s.tabs.iter_mut().find(|t| t.id == active_id) {
                        tab.url = url.clone();
                        if !title.is_empty() {
                            tab.title = title.clone();
                        }
                    }
                }
            }

            let _ = event_tx_clone.try_send(BrowserEvent::PageData {
                url,
                title,
                text: summary,
            });
        });
    }

    debug!("browser watcher task exiting");
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    use crate::manager::BrowserState;

    use super::run_browser_watcher;

    #[tokio::test]
    async fn watcher_exits_on_stop_signal() {
        let state = Arc::new(Mutex::new(BrowserState {
            webview: None,
            latest_snapshot: None,
            watcher_stop: Arc::new(AtomicBool::new(true)),
            event_tx: None,
            tabs: Vec::new(),
            active_tab_id: None,
            sync_fetch_in_progress: Arc::new(AtomicBool::new(false)),
        }));
        let (event_tx, _event_rx) = mpsc::channel(8);
        let stop = Arc::new(AtomicBool::new(true));

        let result = timeout(
            Duration::from_secs(2),
            run_browser_watcher(state, event_tx, stop),
        )
        .await;
        assert!(result.is_ok(), "watcher 应在 stop 信号后退出");
    }

    #[tokio::test]
    async fn watcher_skips_when_no_webview() {
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(BrowserState {
            webview: None,
            latest_snapshot: None,
            watcher_stop: stop.clone(),
            event_tx: None,
            tabs: Vec::new(),
            active_tab_id: None,
            sync_fetch_in_progress: Arc::new(AtomicBool::new(false)),
        }));
        let (event_tx, mut event_rx) = mpsc::channel(8);

        let watcher_state = state.clone();
        let watcher_stop = stop.clone();
        let handle = tokio::spawn(run_browser_watcher(watcher_state, event_tx, watcher_stop));

        tokio::time::sleep(Duration::from_millis(800)).await;
        stop.store(true, Ordering::Relaxed);

        let result = timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "watcher 应在 stop 后退出");

        assert!(event_rx.try_recv().is_err(), "无 webview 时不应发送事件");
    }
}
