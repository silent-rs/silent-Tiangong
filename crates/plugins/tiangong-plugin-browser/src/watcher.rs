//! 浏览器页面自动观察器。
//!
//! 原 `tiangong-core::react::helpers::maybe_inject_browser_update` 的能力下沉实现（#225）。
//! core 不再感知浏览器——本模块在插件内部周期性观察浏览器当前页面，发现变化时经
//! [`PluginFeedbackTx`] 投递 `browser_data` 工具结果，由 core 的 feedback 通道统一注入会话。
//!
//! 与原 core 同步观察的差异（已确认接受）：
//! - 不再在 ReAct 轮次起始 / 每个工具后强制同步观察，改为后台任务按节流周期轮询；
//! - round-0 的「首个模型请求前同步观察」精度不再保证，取决于后台任务与排空时序。
//!
//! 节流策略沿用原 core 逻辑：首次检测无间隔；后续至少间隔 5 秒；URL 变化 / 文本差
//! >500 字符 / 有用户操作 feedback 时才投递，避免无变化刷屏。
//!
//! ## session 隔离
//!
//! [`BrowserWatcher`](crate::watcher::BrowserWatcher) 是 session-scoped，随
//! [`BrowserPlugin`](crate::plugin::BrowserPlugin) 生命周期存在：每个 Core/session 构造
//! 自己的 watcher，只向当前 plugin 持有的 feedback channel 注入 `browser_data`。
//! 不做跨 session 广播——后台/历史 session 不会被无关页面污染。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde_json::json;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tokio::time::sleep;

use crate::capability::PageFetcher;
use crate::types::format_browser_events;

/// 两次自动观察之间的最小间隔。
const MIN_OBSERVE_INTERVAL: Duration = Duration::from_secs(5);
/// 单次 observe_page 的超时（与原 core 一致）。
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(3);
/// 文本长度变化超过该阈值才投递（避免微小抖动）。
const TEXT_DIFF_THRESHOLD: u64 = 500;
/// 后台轮询的 tick 间隔（实际 observe 仍受 MIN_OBSERVE_INTERVAL 节流）。
const POLL_TICK: Duration = Duration::from_secs(2);

/// 浏览器页面观察器：持有 fetcher 与当前 session 的 feedback 通道，后台周期性观察，
/// 变化时只向自己的通道投递 `browser_data`。
///
/// 生命周期由 [`BrowserPlugin`](crate::plugin::BrowserPlugin) 管理：
/// - 构造时创建，但不立即 spawn；
/// - `set_feedback_tx` 注入当前 session 通道时懒启动后台任务（同一实例只启动一次）；
/// - 通道关闭后后台任务检测到并自然跳过（空转等待下次注入）。
pub struct BrowserWatcher {
    fetcher: Arc<dyn PageFetcher>,
    /// 当前 session 的 feedback 通道；未注入时为 None，后台任务空转跳过。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
    /// 后台任务是否已启动（防止 set_feedback_tx 重复调用时重复 spawn）。
    started: AtomicBool,
    /// 上一次观察到的快照（url + 文本长度），用于变化检测。
    last_snapshot: RwLock<Option<(String, usize)>>,
    /// 上一次观察时间，用于节流。
    last_check: RwLock<Option<Instant>>,
}

impl BrowserWatcher {
    pub fn new(fetcher: Arc<dyn PageFetcher>) -> Self {
        Self {
            fetcher,
            feedback_tx: RwLock::new(None),
            started: AtomicBool::new(false),
            last_snapshot: RwLock::new(None),
            last_check: RwLock::new(None),
        }
    }

    /// 注入当前 session 的 feedback 通道并懒启动后台观察任务。
    ///
    /// 可重复调用（如 engine 重建后重新注入通道）：仅更新通道引用，后台任务
    /// 持有的是 watcher 的 `Arc`，会自动读到新通道；`started` 保证只 spawn 一次。
    pub fn set_feedback_tx(self: &Arc<Self>, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.feedback_tx.write() {
            *guard = Some(tx);
        }
        self.ensure_started();
    }

    /// 确保后台任务已启动（同一实例只 spawn 一次）。
    fn ensure_started(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let watcher = self.clone();
        tokio::spawn(async move {
            watcher.run_loop().await;
        });
    }

    async fn run_loop(&self) {
        tracing::debug!("browser watcher started");
        loop {
            sleep(POLL_TICK).await;
            self.maybe_observe_and_inject().await;
        }
    }

    /// 执行一次节流检查 + observe + 变化检测 + 向当前 session 通道投递。
    async fn maybe_observe_and_inject(&self) {
        // 通道未注入或已关闭时跳过（没有会话需要通知）
        let tx = match self.feedback_tx.read() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let Some(tx) = tx else { return };
        if tx.is_closed() {
            return;
        }

        // 节流：首次无限制；后续至少间隔 MIN_OBSERVE_INTERVAL
        let now = Instant::now();
        let has_prev = self
            .last_snapshot
            .read()
            .map(|g| g.is_some())
            .unwrap_or(false);
        if has_prev {
            let too_soon = self
                .last_check
                .read()
                .ok()
                .and_then(|g| g.as_ref().copied())
                .map(|prev| now.duration_since(prev) < MIN_OBSERVE_INTERVAL)
                .unwrap_or(false);
            if too_soon {
                return;
            }
        }
        if let Ok(mut g) = self.last_check.write() {
            *g = Some(now);
        }

        // observe_page（带超时）
        let snapshot =
            match tokio::time::timeout(OBSERVE_TIMEOUT, self.fetcher.observe_page()).await {
                Ok(Some(s)) => s,
                Ok(None) => return,
                Err(_) => {
                    tracing::warn!("browser watcher: observe timeout");
                    return;
                }
            };
        if snapshot.url.is_empty() {
            return;
        }

        let feedback = format_browser_events(&snapshot.events);
        let has_feedback = feedback.is_some();

        // 变化检测：首次必投递；之后看 URL 变化 / 文本差 / 是否有用户操作 feedback
        let prev = self
            .last_snapshot
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(|(u, l)| (u.clone(), *l)));
        let should_inject = match prev {
            None => true,
            Some((prev_url, prev_len)) => {
                has_feedback
                    || prev_url != snapshot.url
                    || (snapshot.text.len() as u64).abs_diff(prev_len as u64) > TEXT_DIFF_THRESHOLD
            }
        };

        let new_state = (snapshot.url.clone(), snapshot.text.len());

        if !should_inject {
            if let Ok(mut g) = self.last_snapshot.write() {
                *g = Some(new_state);
            }
            return;
        }

        // 构造 payload（结构与原 core maybe_inject_browser_update 一致）
        let tabs: Vec<(String, String, String)> = snapshot
            .tabs
            .iter()
            .map(|t| (t.id.clone(), t.url.clone(), t.title.clone()))
            .collect();
        tx.inject_tool(
            "browser_data",
            json!({
                "title": snapshot.title,
                "url": snapshot.url,
                "text": snapshot.text,
                "tabs": tabs,
                "active_tab_id": snapshot.active_tab_id,
                "feedback": feedback,
            }),
        );
        tracing::info!(
            url = %snapshot.url,
            title = %snapshot.title,
            text_len = snapshot.text.len(),
            events_len = snapshot.events.len(),
            has_feedback,
            "browser watcher injected browser_data"
        );

        if let Ok(mut g) = self.last_snapshot.write() {
            *g = Some(new_state);
        }
    }
}
