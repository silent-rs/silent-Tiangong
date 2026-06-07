use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::debug;

use crate::manager::BrowserState;
use crate::types::BrowserEvent;

/// 浏览器监测异步任务。
///
/// 随插件启动常驻，浏览器未打开时跳过检测。
/// 检测 URL 变化后更新活跃标签，并发送轻量级事件（用于前端地址栏更新等）。
/// 页面内容由 on_page_load 回调延迟获取后通过事件通道推送。
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

        // 检测 URL 变化
        if current_url == last_url {
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

        // 发送轻量级 URL 变化事件（用于前端地址栏更新，不含页面内容）
        let _ = event_tx.try_send(BrowserEvent::PageData {
            url: current_url,
            title: String::new(),
            text: String::new(),
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
            page_loaded: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            latest_snapshot: None,
            watcher_stop: Arc::new(AtomicBool::new(true)),
            event_tx: None,
            tabs: Vec::new(),
            active_tab_id: None,
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
            page_loaded: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            latest_snapshot: None,
            watcher_stop: stop.clone(),
            event_tx: None,
            tabs: Vec::new(),
            active_tab_id: None,
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
