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
//! ## 多 session 广播
//!
//! GUI 入口为每个 Core（session）构造独立的 [`BrowserPlugin`](crate::plugin::BrowserPlugin)，
//! 但它们共享同一个内嵌浏览器。为避免多个 watcher 重复 observe 同一页面，
//! 观察任务通过 [`BROWSER_WATCHER`] 进程级单例保证只 spawn 一次；各 session 的
//! feedback 通道注册到同一个 watcher，observe 到的变化广播给全部活跃通道。

use std::sync::{Arc, OnceLock, RwLock};
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

/// 进程级单例 watcher：保证全局只有一个后台 observe 任务。
///
/// 首次 [`BrowserWatcher::ensure_started`] 时初始化并 spawn；后续调用复用同一实例，
/// 仅把新的 feedback 通道追加到广播列表。这样多 session 共享一个浏览器时不会重复 observe。
static BROWSER_WATCHER: OnceLock<RwLock<Option<Arc<BrowserWatcher>>>> = OnceLock::new();

fn watcher_slot() -> &'static RwLock<Option<Arc<BrowserWatcher>>> {
    BROWSER_WATCHER.get_or_init(|| RwLock::new(None))
}

/// 浏览器页面观察器：持有 fetcher，后台周期性观察，变化时向全部注册通道广播。
pub struct BrowserWatcher {
    fetcher: Arc<dyn PageFetcher>,
    /// 已注册的 feedback 通道列表（每个 session/Core 一个）。通道关闭后从列表移除。
    channels: RwLock<Vec<PluginFeedbackTx>>,
    /// 上一次观察到的快照（url + 文本长度），用于变化检测。
    last_snapshot: RwLock<Option<(String, usize)>>,
    /// 上一次观察时间，用于节流。
    last_check: RwLock<Option<Instant>>,
}

impl BrowserWatcher {
    /// 获取或创建进程级单例，并确保后台任务已启动。
    ///
    /// 首次调用时用传入的 fetcher 初始化；后续调用（其他 session 构造 BrowserPlugin 时）
    /// 复用已有实例——fetcher 参数被忽略（浏览器全局唯一，fetcher 等价）。
    /// 返回单例 Arc，供 [`BrowserWatcher::register_channel`] 注册当前 session 通道。
    pub fn ensure_started(fetcher: Arc<dyn PageFetcher>) -> Arc<Self> {
        // 快速路径：已存在
        if let Ok(read) = watcher_slot().read() {
            if let Some(existing) = read.as_ref() {
                return existing.clone();
            }
        }
        // 慢速路径：加写锁创建
        let mut write = watcher_slot()
            .write()
            .expect("browser watcher slot poisoned");
        if let Some(existing) = write.as_ref() {
            return existing.clone();
        }
        let watcher = Arc::new(BrowserWatcher {
            fetcher,
            channels: RwLock::new(Vec::new()),
            last_snapshot: RwLock::new(None),
            last_check: RwLock::new(None),
        });
        let cloned = watcher.clone();
        tokio::spawn(async move {
            cloned.run_loop().await;
        });
        *write = Some(watcher.clone());
        watcher
    }

    /// 注册一个 feedback 通道（每个 session 在 `set_feedback_tx` 时调用）。
    pub fn register_channel(&self, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.channels.write() {
            guard.push(tx);
        }
    }

    async fn run_loop(&self) {
        tracing::debug!("browser watcher started");
        loop {
            sleep(POLL_TICK).await;
            self.maybe_observe_and_broadcast().await;
        }
    }

    /// 执行一次节流检查 + observe + 变化检测 + 向全部活跃通道广播。
    async fn maybe_observe_and_broadcast(&self) {
        // 无注册通道时跳过（没有会话需要通知）
        let channel_count = self.channels.read().map(|g| g.len()).unwrap_or(0);
        if channel_count == 0 {
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
        let payload = json!({
            "title": snapshot.title,
            "url": snapshot.url,
            "text": snapshot.text,
            "tabs": tabs,
            "active_tab_id": snapshot.active_tab_id,
            "feedback": feedback,
        });

        // 广播给全部活跃通道，清理已关闭的
        let channels: Vec<PluginFeedbackTx> =
            self.channels.read().map(|g| g.clone()).unwrap_or_default();
        let mut alive = Vec::with_capacity(channels.len());
        for tx in channels {
            if tx.is_closed() {
                continue;
            }
            tx.inject_tool("browser_data", payload.clone());
            alive.push(tx);
        }
        tracing::info!(
            url = %snapshot.url,
            title = %snapshot.title,
            text_len = snapshot.text.len(),
            events_len = snapshot.events.len(),
            has_feedback,
            broadcast_to = alive.len(),
            "browser watcher injected browser_data"
        );

        if let Ok(mut g) = self.channels.write() {
            *g = alive;
        }
        if let Ok(mut g) = self.last_snapshot.write() {
            *g = Some(new_state);
        }
    }
}
