//! Agent 投递的持久取消与完成协议账本。
//!
//! 父 Session 的 pending 投递由 Core 原子提交，但取消命令可能在确认返回前遇到
//! 进程崩溃。插件先把稳定投递 ID 写入自己的会话目录，重启时据此只重放取消，
//! 不会把用户已经取消的工作重新交给子 Agent 执行。父 Session 确认后，ID 被
//! 原子移动到永久 `settled_ids`，避免父完成账本淘汰后重新播放 child receipt。

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tiangong_core::core::plugin::PluginFeedbackTx;

use crate::state::message_bus::AgentInboxEntry;

/// 尚未由目标 Agent 消费的内部团队消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DurableInternalDelivery {
    pub(crate) target_agent_id: String,
    pub(crate) entry: AgentInboxEntry,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DeliveryProtocolFile {
    /// 新版显式取消集合。
    #[serde(default)]
    cancelled_ids: BTreeSet<String>,
    /// 父 Session 已持久确认的永久完成集合。
    #[serde(default)]
    settled_ids: BTreeSet<String>,
    /// 已在团队内存收件箱入队、但尚未完成的内部消息。
    #[serde(default)]
    pending_internal_deliveries: BTreeMap<String, DurableInternalDelivery>,
    /// 兼容旧版 `agent-team-cancellations.json`。
    #[serde(default, skip_serializing)]
    delivery_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DeliveryProtocolState {
    pub(crate) cancelled_ids: BTreeSet<String>,
    pub(crate) settled_ids: BTreeSet<String>,
    pub(crate) pending_internal_deliveries: BTreeMap<String, DurableInternalDelivery>,
}

impl DeliveryProtocolFile {
    fn into_state(self) -> DeliveryProtocolState {
        let mut cancelled_ids = self.cancelled_ids;
        cancelled_ids.extend(self.delivery_ids);
        cancelled_ids.retain(|delivery_id| !self.settled_ids.contains(delivery_id));
        let mut pending_internal_deliveries = self.pending_internal_deliveries;
        pending_internal_deliveries.retain(|delivery_id, _| {
            !cancelled_ids.contains(delivery_id) && !self.settled_ids.contains(delivery_id)
        });
        DeliveryProtocolState {
            cancelled_ids,
            settled_ids: self.settled_ids,
            pending_internal_deliveries,
        }
    }
}

impl From<&DeliveryProtocolState> for DeliveryProtocolFile {
    fn from(state: &DeliveryProtocolState) -> Self {
        Self {
            cancelled_ids: state.cancelled_ids.clone(),
            settled_ids: state.settled_ids.clone(),
            pending_internal_deliveries: state.pending_internal_deliveries.clone(),
            delivery_ids: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CancellationTombstoneStore {
    storage_root: PathBuf,
    /// clone 后仍共享的 RMW 门闩，避免并发 record/remove 互相覆盖。
    update_gate: Arc<Mutex<()>>,
}

impl CancellationTombstoneStore {
    pub(crate) fn new(storage_root: PathBuf) -> Self {
        Self {
            storage_root,
            update_gate: Arc::new(Mutex::new(())),
        }
    }

    fn path(&self, session_id: &str) -> PathBuf {
        self.storage_root
            .join("sessions")
            .join(session_id)
            .join("agent-team-cancellations.json")
    }

    pub(crate) fn load_state(&self, session_id: &str) -> Result<DeliveryProtocolState, String> {
        let _guard = self
            .update_gate
            .lock()
            .map_err(|_| "Agent 投递取消记录锁定失败".to_string())?;
        self.load_state_unlocked(session_id)
    }

    fn load_state_unlocked(&self, session_id: &str) -> Result<DeliveryProtocolState, String> {
        let path = self.path(session_id);
        if !path.exists() {
            return Ok(DeliveryProtocolState::default());
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|error| format!("读取 Agent 投递取消记录失败：{error}"))?;
        serde_json::from_str::<DeliveryProtocolFile>(&content)
            .map(DeliveryProtocolFile::into_state)
            .map_err(|error| format!("解析 Agent 投递取消记录失败：{error}"))
    }

    pub(crate) fn record_cancelled(
        &self,
        session_id: &str,
        delivery_ids: impl IntoIterator<Item = String>,
    ) -> Result<DeliveryProtocolState, String> {
        let delivery_ids = delivery_ids.into_iter().collect::<BTreeSet<_>>();
        self.update_state(session_id, move |state| {
            for delivery_id in delivery_ids {
                if !state.settled_ids.contains(&delivery_id) {
                    state.cancelled_ids.insert(delivery_id);
                }
            }
        })
    }

    /// 在内部消息对调用方可见前，先把完整收件箱 entry 原子写入协议账本。
    ///
    /// 已取消或已结算的稳定 ID 属于终态，不能重新进入待处理集合。
    pub(crate) fn record_internal_deliveries(
        &self,
        session_id: &str,
        deliveries: impl IntoIterator<Item = (String, AgentInboxEntry)>,
    ) -> Result<DeliveryProtocolState, String> {
        let deliveries = deliveries
            .into_iter()
            .map(|(target_agent_id, entry)| {
                (
                    entry.message.id.clone(),
                    DurableInternalDelivery {
                        target_agent_id,
                        entry,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        if deliveries.is_empty() {
            return self.load_state(session_id);
        }

        let _guard = self
            .update_gate
            .lock()
            .map_err(|_| "Agent 投递取消记录锁定失败".to_string())?;
        let mut state = self.load_state_unlocked(session_id)?;
        let mut changed = false;
        for (delivery_id, delivery) in deliveries {
            if state.cancelled_ids.contains(&delivery_id)
                || state.settled_ids.contains(&delivery_id)
            {
                continue;
            }
            state
                .pending_internal_deliveries
                .insert(delivery_id, delivery);
            changed = true;
        }
        if changed {
            self.persist_state(session_id, &state)?;
        }
        Ok(state)
    }

    /// 把父 Session 已确认的投递永久结算，并在同一次原子替换中删除取消 tombstone。
    pub(crate) fn settle(
        &self,
        session_id: &str,
        delivery_ids: &[String],
    ) -> Result<DeliveryProtocolState, String> {
        let delivery_ids = delivery_ids.iter().cloned().collect::<BTreeSet<_>>();
        self.update_state(session_id, move |state| {
            for delivery_id in delivery_ids {
                state.cancelled_ids.remove(&delivery_id);
                state.settled_ids.insert(delivery_id);
            }
        })
    }

    /// 父 Session ACK 后持续重试永久结算。返回 `false` 表示关闭边界已到，receipt
    /// 与父完成账本仍保留，下一次会话恢复可继续完成结算。
    pub(crate) async fn settle_with_retry(
        &self,
        session_id: &str,
        delivery_ids: &[String],
        stopping: &AtomicBool,
        feedback: &PluginFeedbackTx,
    ) -> bool {
        if delivery_ids.is_empty() {
            return true;
        }
        let mut retry_delay = Duration::from_millis(100);
        loop {
            match self.settle(session_id, delivery_ids) {
                Ok(_) => return true,
                Err(error) => {
                    tracing::warn!(%error, "持久化 Agent 投递永久结算失败，稍后重试");
                }
            }
            if stopping.load(Ordering::Acquire) || feedback.is_closed() {
                return false;
            }
            tokio::time::sleep(retry_delay).await;
            retry_delay = (retry_delay * 2).min(Duration::from_secs(2));
        }
    }

    fn update_state(
        &self,
        session_id: &str,
        mutate: impl FnOnce(&mut DeliveryProtocolState),
    ) -> Result<DeliveryProtocolState, String> {
        let _guard = self
            .update_gate
            .lock()
            .map_err(|_| "Agent 投递取消记录锁定失败".to_string())?;
        let mut state = self.load_state_unlocked(session_id)?;
        mutate(&mut state);
        state
            .cancelled_ids
            .retain(|delivery_id| !state.settled_ids.contains(delivery_id));
        state.pending_internal_deliveries.retain(|delivery_id, _| {
            !state.cancelled_ids.contains(delivery_id) && !state.settled_ids.contains(delivery_id)
        });
        self.persist_state(session_id, &state)?;
        Ok(state)
    }

    fn persist_state(&self, session_id: &str, state: &DeliveryProtocolState) -> Result<(), String> {
        let path = self.path(session_id);
        let parent = path
            .parent()
            .ok_or_else(|| format!("取消记录路径缺少父目录：{}", path.display()))?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建 Agent 取消记录目录失败：{error}"))?;
        let content = serde_json::to_vec_pretty(&DeliveryProtocolFile::from(state))
            .map_err(|error| format!("序列化 Agent 投递取消记录失败：{error}"))?;
        tiangong_core::session::atomic_replace_file(&path, &content)
            .map_err(|error| format!("写入 Agent 投递取消记录失败：{error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::state::{AgentMessage, MessagePriority};

    fn internal_entry(delivery_id: &str, target_agent_id: &str) -> AgentInboxEntry {
        AgentInboxEntry {
            message: AgentMessage {
                id: delivery_id.to_string(),
                from: "agent-source".to_string(),
                to: target_agent_id.to_string(),
                content: format!("work for {target_agent_id}"),
                priority: MessagePriority::Normal,
                created_at: "2026-07-12 12:00:00".to_string(),
            },
            additional_content: Vec::new(),
            session_message_id: None,
        }
    }

    #[test]
    fn tombstones_survive_restart_and_move_to_settled_after_ack() {
        let root = tempfile::tempdir().unwrap();
        let store = CancellationTombstoneStore::new(root.path().to_path_buf());

        store
            .record_cancelled(
                "session-1",
                ["delivery-2".to_string(), "delivery-1".to_string()],
            )
            .unwrap();
        let restored = CancellationTombstoneStore::new(root.path().to_path_buf())
            .load_state("session-1")
            .unwrap();
        assert_eq!(
            restored.cancelled_ids.into_iter().collect::<Vec<_>>(),
            ["delivery-1", "delivery-2"]
        );

        store
            .settle("session-1", &["delivery-1".to_string()])
            .unwrap();
        let state = store.load_state("session-1").unwrap();
        assert_eq!(
            state.cancelled_ids.into_iter().collect::<Vec<_>>(),
            ["delivery-2"]
        );
        assert_eq!(
            state.settled_ids.into_iter().collect::<Vec<_>>(),
            ["delivery-1"]
        );
    }

    #[test]
    fn path_is_scoped_to_plugin_session_storage() {
        let store = CancellationTombstoneStore::new(Path::new("/tmp/root").to_path_buf());
        assert_eq!(
            store.path("session-1"),
            Path::new("/tmp/root/sessions/session-1/agent-team-cancellations.json")
        );
    }

    #[test]
    fn cloned_stores_serialize_concurrent_updates_without_losing_ids() {
        let root = tempfile::tempdir().unwrap();
        let store = CancellationTombstoneStore::new(root.path().to_path_buf());
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let handles = (0..16)
            .map(|index| {
                let store = store.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .record_cancelled("session-1", [format!("delivery-{index}")])
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            store.load_state("session-1").unwrap().cancelled_ids.len(),
            16
        );
    }

    #[test]
    fn legacy_delivery_ids_are_loaded_and_rewritten_with_new_fields() {
        let root = tempfile::tempdir().unwrap();
        let store = CancellationTombstoneStore::new(root.path().to_path_buf());
        let path = store.path("session-1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"delivery_ids":["legacy-1","legacy-2"]}"#).unwrap();

        let restored = store.load_state("session-1").unwrap();
        assert_eq!(
            restored.cancelled_ids.into_iter().collect::<Vec<_>>(),
            ["legacy-1", "legacy-2"]
        );
        assert!(restored.settled_ids.is_empty());

        store
            .record_cancelled("session-1", ["current-1".to_string()])
            .unwrap();
        let rewritten: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(rewritten.get("delivery_ids").is_none());
        assert!(rewritten.get("cancelled_ids").is_some());
        assert!(rewritten.get("settled_ids").is_some());
        assert!(rewritten.get("pending_internal_deliveries").is_some());
    }

    #[test]
    fn internal_deliveries_round_trip_and_terminal_ids_cannot_reenter() {
        let root = tempfile::tempdir().unwrap();
        let store = CancellationTombstoneStore::new(root.path().to_path_buf());
        store
            .record_internal_deliveries(
                "session-1",
                [
                    (
                        "agent-dev".to_string(),
                        internal_entry("delivery-1", "agent-dev"),
                    ),
                    (
                        "agent-test".to_string(),
                        internal_entry("delivery-2", "agent-test"),
                    ),
                ],
            )
            .unwrap();

        let restored = CancellationTombstoneStore::new(root.path().to_path_buf())
            .load_state("session-1")
            .unwrap();
        let delivery = restored
            .pending_internal_deliveries
            .get("delivery-1")
            .unwrap();
        assert_eq!(delivery.target_agent_id, "agent-dev");
        assert_eq!(delivery.entry.message.content, "work for agent-dev");
        assert!(delivery.entry.session_message_id.is_none());

        store
            .record_cancelled("session-1", ["delivery-1".to_string()])
            .unwrap();
        store
            .settle("session-1", &["delivery-2".to_string()])
            .unwrap();
        let state = store.load_state("session-1").unwrap();
        assert!(state.pending_internal_deliveries.is_empty());
        assert!(state.cancelled_ids.contains("delivery-1"));
        assert!(state.settled_ids.contains("delivery-2"));

        let cancelled_reentry = store
            .record_internal_deliveries(
                "session-1",
                [(
                    "agent-dev".to_string(),
                    internal_entry("delivery-1", "agent-dev"),
                )],
            )
            .unwrap();
        let settled_reentry = store
            .record_internal_deliveries(
                "session-1",
                [(
                    "agent-test".to_string(),
                    internal_entry("delivery-2", "agent-test"),
                )],
            )
            .unwrap();
        assert!(!cancelled_reentry
            .pending_internal_deliveries
            .contains_key("delivery-1"));
        assert!(!settled_reentry
            .pending_internal_deliveries
            .contains_key("delivery-2"));
        assert!(store
            .load_state("session-1")
            .unwrap()
            .pending_internal_deliveries
            .is_empty());
    }

    #[test]
    fn settling_moves_cancelled_ids_and_prevents_re_recording() {
        let root = tempfile::tempdir().unwrap();
        let store = CancellationTombstoneStore::new(root.path().to_path_buf());
        store
            .record_cancelled(
                "session-1",
                ["delivery-1".to_string(), "delivery-2".to_string()],
            )
            .unwrap();

        store
            .settle("session-1", &["delivery-1".to_string()])
            .unwrap();
        let state = store
            .record_cancelled(
                "session-1",
                ["delivery-1".to_string(), "delivery-3".to_string()],
            )
            .unwrap();

        assert_eq!(
            state.cancelled_ids.into_iter().collect::<Vec<_>>(),
            ["delivery-2", "delivery-3"]
        );
        assert_eq!(
            state.settled_ids.into_iter().collect::<Vec<_>>(),
            ["delivery-1"]
        );
    }

    #[test]
    fn concurrent_record_and_settle_always_leave_ids_settled() {
        let root = tempfile::tempdir().unwrap();
        let store = CancellationTombstoneStore::new(root.path().to_path_buf());
        let barrier = Arc::new(std::sync::Barrier::new(32));
        let mut handles = Vec::new();
        for index in 0..16 {
            let delivery_id = format!("delivery-{index}");
            let record_store = store.clone();
            let record_barrier = Arc::clone(&barrier);
            let record_id = delivery_id.clone();
            handles.push(std::thread::spawn(move || {
                record_barrier.wait();
                record_store
                    .record_cancelled("session-1", [record_id])
                    .unwrap();
            }));

            let settle_store = store.clone();
            let settle_barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                settle_barrier.wait();
                settle_store.settle("session-1", &[delivery_id]).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let state = store.load_state("session-1").unwrap();
        assert!(state.cancelled_ids.is_empty());
        assert_eq!(state.settled_ids.len(), 16);
    }

    #[test]
    fn concurrent_internal_record_and_terminal_updates_never_resurrect_work() {
        let root = tempfile::tempdir().unwrap();
        let store = CancellationTombstoneStore::new(root.path().to_path_buf());
        let barrier = Arc::new(std::sync::Barrier::new(32));
        let mut handles = Vec::new();
        for index in 0..16 {
            let delivery_id = format!("internal-{index}");
            let target_agent_id = format!("agent-{index}");
            let record_store = store.clone();
            let record_barrier = Arc::clone(&barrier);
            let record_id = delivery_id.clone();
            handles.push(std::thread::spawn(move || {
                record_barrier.wait();
                record_store
                    .record_internal_deliveries(
                        "session-1",
                        [(
                            target_agent_id.clone(),
                            internal_entry(&record_id, &target_agent_id),
                        )],
                    )
                    .unwrap();
            }));

            let terminal_store = store.clone();
            let terminal_barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                terminal_barrier.wait();
                if index % 2 == 0 {
                    terminal_store.settle("session-1", &[delivery_id]).unwrap();
                } else {
                    terminal_store
                        .record_cancelled("session-1", [delivery_id])
                        .unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let state = store.load_state("session-1").unwrap();
        assert!(state.pending_internal_deliveries.is_empty());
        assert_eq!(state.settled_ids.len(), 8);
        assert_eq!(state.cancelled_ids.len(), 8);
    }
}
