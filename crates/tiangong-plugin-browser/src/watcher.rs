use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::debug;

use crate::manager::BrowserState;
use crate::types::{BrowserCommand, BrowserEvent};

/// 浏览器监测异步任务。
///
/// 随插件启动常驻，浏览器未打开时跳过检测。
/// 检测到 URL 变化后通过 ObservePage 命令获取页面内容，
/// 再通过 event_tx 统一发出 BrowserEvent。
pub async fn run_browser_watcher(
    state: Arc<Mutex<BrowserState>>,
    cmd_tx: mpsc::Sender<BrowserCommand>,
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
                        _ => {
                            // URL 读取失败（wry 内部异常），跳过本轮
                            continue;
                        }
                    }
                }
                None => {
                    // 浏览器未打开，休眠等待
                    continue;
                }
            }
        };

        // 检测 URL 变化
        let url_changed = if current_url != last_url {
            last_url = current_url.clone();
            true
        } else {
            false
        };

        if !url_changed {
            continue;
        }

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

        // 通过命令通道获取页面内容
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        if cmd_tx
            .send(BrowserCommand::ObservePage { response_tx })
            .await
            .is_err()
        {
            continue;
        }

        let snapshot = match tokio::time::timeout(Duration::from_secs(10), response_rx).await {
            Ok(Ok(s)) => s,
            _ => {
                // 获取快照超时或失败，仍发送 URL 变化事件（无内容）
                let _ = event_tx
                    .send(BrowserEvent::PageLoaded {
                        url: current_url,
                        title: String::new(),
                        text: String::new(),
                    })
                    .await;
                continue;
            }
        };

        let _ = event_tx
            .send(BrowserEvent::PageLoaded {
                url: snapshot.url.clone(),
                title: snapshot.title,
                text: snapshot.text.chars().take(2000).collect(),
            })
            .await;
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
        let (cmd_tx, _cmd_rx) = mpsc::channel(8);
        let (event_tx, _event_rx) = mpsc::channel(8);
        let stop = Arc::new(AtomicBool::new(true));

        // watcher 应在首个 tick 检测到 stop 后立即退出
        let result = timeout(
            Duration::from_secs(2),
            run_browser_watcher(state, cmd_tx, event_tx, stop),
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
        let (cmd_tx, _cmd_rx) = mpsc::channel(8);
        let (event_tx, mut event_rx) = mpsc::channel(8);

        // 启动 watcher，1 秒后设置 stop
        let watcher_state = state.clone();
        let watcher_stop = stop.clone();
        let handle = tokio::spawn(run_browser_watcher(
            watcher_state,
            cmd_tx,
            event_tx,
            watcher_stop,
        ));

        tokio::time::sleep(Duration::from_millis(800)).await;
        stop.store(true, Ordering::Relaxed);

        let result = timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "watcher 应在 stop 后退出");

        // 无 webview 时不应产生任何事件
        assert!(event_rx.try_recv().is_err(), "无 webview 时不应发送事件");
    }
}
