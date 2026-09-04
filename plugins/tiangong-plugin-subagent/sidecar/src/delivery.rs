//! Hook 投递：把持久化的重要反馈经本机 server 消息通道注入激活会话。
//!
//! 可靠性约定：事件先由 RuntimeStore 落盘（hooks/queue/），本 worker 轮询
//! 投递，成功即出队；失败退避重试；server 未启动时低频等待，不丢事件。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Mutex;

use tiangong_plugin_subagent_protocol::hooks::HookEvent;

use crate::runtime_store::RuntimeStore;

const SERVER_URL_ENV: &str = "TIANGONG_SERVER_URL";
const SERVER_TOKEN_ENV: &str = "TIANGONG_SERVER_TOKEN";

/// 正常轮询间隔（投递成功后）。
const IDLE_INTERVAL: Duration = Duration::from_secs(2);
/// 投递失败后的退避间隔。
const RETRY_INTERVAL: Duration = Duration::from_secs(15);
/// 单次 HTTP 请求超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Hook 投递工作器（sidecar 启动时 spawn，单实例串行投递）。
pub struct DeliveryWorker {
    store: Arc<RuntimeStore>,
    client: reqwest::Client,
    /// 测试/关闭时停止循环。
    running: Arc<Mutex<bool>>,
}

impl DeliveryWorker {
    pub fn start(store: Arc<RuntimeStore>) -> Arc<Self> {
        let worker = Arc::new(Self {
            store,
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("构建 reqwest 客户端失败"),
            running: Arc::new(Mutex::new(true)),
        });
        let loop_worker = Arc::clone(&worker);
        let loop_running = Arc::clone(&worker.running);
        tokio::spawn(async move {
            while *loop_running.lock().await {
                let delay = loop_worker.deliver_pending().await;
                tokio::time::sleep(delay).await;
            }
        });
        worker
    }

    /// 投递队列中全部待投递事件，返回下次轮询间隔。
    async fn deliver_pending(&self) -> Duration {
        let mut hooks = self.store.queued_hooks();
        if hooks.is_empty() {
            return IDLE_INTERVAL;
        }
        // 串行按事件时间序投递（event_id 为 scru128，字典序即时间序）。
        hooks.sort_by(|a, b| a.event_id.cmp(&b.event_id));
        let mut any_failure = false;
        for mut event in hooks {
            match self.deliver_one(&event).await {
                Ok(()) => {
                    if let Err(error) = self.store.dequeue_hook(&event.event_id) {
                        tracing::warn!(event_id = %event.event_id, %error, "移除已投递 Hook 失败");
                    }
                }
                Err(error) => {
                    any_failure = true;
                    event.attempts = event.attempts.saturating_add(1);
                    event.last_error = Some(error.to_string());
                    if let Err(update_error) = self.store.update_hook(&event) {
                        tracing::warn!(event_id = %event.event_id, %update_error, "更新 Hook 投递状态失败");
                    }
                    tracing::warn!(event_id = %event.event_id, %error, "Hook 投递失败，稍后重试");
                }
            }
        }
        if any_failure {
            RETRY_INTERVAL
        } else {
            IDLE_INTERVAL
        }
    }

    /// 单事件投递：POST /api/v1/messages（respond-async，202 即成功）。
    async fn deliver_one(&self, event: &HookEvent) -> Result<()> {
        let url = std::env::var(SERVER_URL_ENV).unwrap_or_default();
        if url.is_empty() {
            anyhow::bail!(
                "天工 Server 未启动，消息暂无法投递：请先在天工设置中开启 Server（或从托盘菜单启动），必要时重启天工后重试"
            );
        }
        let endpoint = format!("{}/api/v1/messages", url.trim_end_matches('/'));
        let mut request = self
            .client
            .post(&endpoint)
            .header("Prefer", "respond-async")
            .json(&serde_json::json!({
                "connector": "server-api",
                "channel_id": event.conversation_id,
                "message": event.render_message(),
            }));
        if let Ok(token) = std::env::var(SERVER_TOKEN_ENV)
            && !token.is_empty()
        {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() || status.as_u16() == 202 {
            tracing::info!(event_id = %event.event_id, event_type = ?event.event_type, "Hook 已投递到会话");
            Ok(())
        } else {
            anyhow::bail!("server 返回 {status}");
        }
    }
}

/// 向指定会话投递一条用户可见消息（respond-async；Hook 队列与会话后端共用）。
pub async fn deliver_message(
    client: &reqwest::Client,
    conversation_id: &str,
    message: &str,
) -> Result<()> {
    let url = std::env::var(SERVER_URL_ENV).unwrap_or_default();
    if url.is_empty() {
        anyhow::bail!(
            "天工 Server 未启动，消息暂无法投递：请先在天工设置中开启 Server（或从托盘菜单启动），必要时重启天工后重试"
        );
    }
    let endpoint = format!("{}/api/v1/messages", url.trim_end_matches('/'));
    let mut request = client
        .post(&endpoint)
        .header("Prefer", "respond-async")
        .json(&serde_json::json!({
            "connector": "server-api",
            "channel_id": conversation_id,
            "message": message,
        }));
    if let Ok(token) = std::env::var(SERVER_TOKEN_ENV)
        && !token.is_empty()
    {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    let status = response.status();
    if status.is_success() || status.as_u16() == 202 {
        Ok(())
    } else {
        anyhow::bail!("server 返回 {status}");
    }
}
