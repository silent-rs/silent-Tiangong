use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tauri::{AppHandle, Emitter, Manager};
use tiangong_core::permission::TrustMode;
use tiangong_core::session::{MessageRole, Session};
use tiangong_server::remote::backend::{CoreBackendKind, ServerCoreBackend};
use tiangong_server::remote::event::{EventBus, TiangongEvent};
use tiangong_types::{MediaAsset, OutgoingMessage, TurnStatus};
use tokio::sync::{mpsc, oneshot};

use crate::app::TiangongApp;

type HostResult<T> = std::result::Result<T, String>;
type MessageReply = HostResult<(String, OutgoingMessage)>;

#[derive(Default)]
pub(crate) struct RemoteTurnCorrelation {
    finalized_user_message_id: Option<String>,
}

struct RemoteTurnLease<'a> {
    state: &'a TiangongApp,
    session_id: String,
    message_id: String,
}

impl Drop for RemoteTurnLease<'_> {
    fn drop(&mut self) {
        self.state
            .finish_remote_turn(&self.session_id, &self.message_id);
    }
}

impl RemoteTurnCorrelation {
    /// Core 保证用户终态快照先于该轮 Done/Error。这里把两者组合成稳定消息 ID，
    /// 后续轮次即使已经排队，也不会按“最后一条消息”误取回复。
    pub(crate) fn observe(&mut self, event: &tiangong_types::StreamEvent) -> Option<String> {
        if let tiangong_types::StreamEvent::SessionMessageUpsert { message, .. } = event {
            if message.role == tiangong_types::MessageRole::User && message.turn_status.is_some() {
                self.finalized_user_message_id = Some(message.id.clone());
                return None;
            }
        }
        if matches!(
            event,
            tiangong_types::StreamEvent::Done { .. } | tiangong_types::StreamEvent::Error { .. }
        ) {
            return self.finalized_user_message_id.take();
        }
        None
    }
}

enum EmbeddedCoreRequest {
    SendConnector {
        connector: String,
        channel_id: String,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
        reply: oneshot::Sender<MessageReply>,
    },
    SendSession {
        session_id: String,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
        reply: oneshot::Sender<MessageReply>,
    },
    DeleteSession {
        session_id: String,
        reply: oneshot::Sender<HostResult<bool>>,
    },
    SyncConfig {
        reply: oneshot::Sender<HostResult<()>>,
    },
}

/// 内嵌 HTTP 线程只持有请求通道，不持有 Core 或会话写入器。
pub(crate) struct DesktopServerCoreBridge {
    request_tx: mpsc::UnboundedSender<EmbeddedCoreRequest>,
}

impl DesktopServerCoreBridge {
    async fn request_message(
        &self,
        request: impl FnOnce(oneshot::Sender<MessageReply>) -> EmbeddedCoreRequest,
    ) -> Result<(String, OutgoingMessage)> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send(request(reply_tx))
            .map_err(|_| anyhow!("Desktop 内嵌 Server 桥接已关闭"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("Desktop 内嵌 Server 未返回处理结果"))?
            .map_err(anyhow::Error::msg)
    }
}

#[async_trait]
impl ServerCoreBackend for DesktopServerCoreBridge {
    fn kind(&self) -> CoreBackendKind {
        CoreBackendKind::EmbeddedHost
    }

    async fn send_connector_message_and_wait(
        &self,
        connector: &str,
        channel_id: &str,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
    ) -> Result<(String, OutgoingMessage)> {
        self.request_message(|reply| EmbeddedCoreRequest::SendConnector {
            connector: connector.to_string(),
            channel_id: channel_id.to_string(),
            content,
            message_id,
            media,
            reply,
        })
        .await
    }

    async fn send_message_and_wait(
        &self,
        session_id: &str,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
    ) -> Result<(String, OutgoingMessage)> {
        self.request_message(|reply| EmbeddedCoreRequest::SendSession {
            session_id: session_id.to_string(),
            content,
            message_id,
            media,
            reply,
        })
        .await
    }

    async fn delete_session(&self, session_id: &str) -> Result<bool> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send(EmbeddedCoreRequest::DeleteSession {
                session_id: session_id.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| anyhow!("Desktop 内嵌 Server 桥接已关闭"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("Desktop 内嵌 Server 未返回删除结果"))?
            .map_err(anyhow::Error::msg)
    }

    async fn sync_config_from_state(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send(EmbeddedCoreRequest::SyncConfig { reply: reply_tx })
            .map_err(|_| anyhow!("Desktop 内嵌 Server 桥接已关闭"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("Desktop 内嵌 Server 未返回配置同步结果"))?
            .map_err(anyhow::Error::msg)
    }
}

pub(crate) fn spawn_desktop_server_core_bridge(
    app: AppHandle,
    event_bus: Arc<EventBus>,
) -> Arc<DesktopServerCoreBridge> {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    tauri::async_runtime::spawn(async move {
        while let Some(request) = request_rx.recv().await {
            let request_app = app.clone();
            let request_event_bus = event_bus.clone();
            tauri::async_runtime::spawn(async move {
                match request {
                    EmbeddedCoreRequest::SendConnector {
                        connector,
                        channel_id,
                        content,
                        message_id,
                        media,
                        reply,
                    } => {
                        let state = request_app.state::<TiangongApp>();
                        let result = match resolve_connector_session(
                            &request_app,
                            state.inner(),
                            &request_event_bus,
                            &connector,
                            &channel_id,
                        )
                        .await
                        {
                            Ok(session_id) => {
                                let completion_session_id = session_id.clone();
                                let result = send_message_and_wait(
                                    request_app.clone(),
                                    state.inner(),
                                    session_id,
                                    content,
                                    message_id,
                                    media,
                                )
                                .await;
                                request_event_bus.publish(TiangongEvent::TurnCompleted {
                                    session_id: completion_session_id,
                                    success: result.is_ok(),
                                });
                                result
                            }
                            Err(error) => Err(error),
                        };
                        let _ = reply.send(result);
                    }
                    EmbeddedCoreRequest::SendSession {
                        session_id,
                        content,
                        message_id,
                        media,
                        reply,
                    } => {
                        let state = request_app.state::<TiangongApp>();
                        let completion_session_id = session_id.clone();
                        let result = send_message_and_wait(
                            request_app.clone(),
                            state.inner(),
                            session_id,
                            content,
                            message_id,
                            media,
                        )
                        .await;
                        request_event_bus.publish(TiangongEvent::TurnCompleted {
                            session_id: completion_session_id,
                            success: result.is_ok(),
                        });
                        let _ = reply.send(result);
                    }
                    EmbeddedCoreRequest::DeleteSession { session_id, reply } => {
                        let state = request_app.state::<TiangongApp>();
                        let result = delete_session(&request_app, state.inner(), &session_id).await;
                        let _ = reply.send(result);
                    }
                    EmbeddedCoreRequest::SyncConfig { reply } => {
                        let state = request_app.state::<TiangongApp>();
                        let result = state.sync_core_config_from_state().await;
                        if result.is_ok() {
                            request_event_bus.publish(TiangongEvent::ConfigChanged);
                        }
                        let _ = reply.send(result);
                    }
                }
            });
        }
    });
    Arc::new(DesktopServerCoreBridge { request_tx })
}

async fn resolve_connector_session(
    app: &AppHandle,
    state: &TiangongApp,
    event_bus: &EventBus,
    connector: &str,
    channel_id: &str,
) -> HostResult<String> {
    let channel_id = channel_id.trim();
    if channel_id.is_empty() {
        return state
            .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
            .await;
    }

    let connector = connector.trim();
    let title = if connector.is_empty() {
        format!("外部通道 {channel_id}")
    } else {
        format!("{connector} {channel_id}")
    }
    .chars()
    .take(80)
    .collect::<String>();
    let (session_id, created) = state
        .with_state(|core_state| {
            if let Some(session) = core_state
                .sessions()
                .iter()
                .find(|session| session.id == channel_id)
            {
                return Ok((session.id.clone(), false));
            }
            if let Some(session) = core_state
                .sessions()
                .iter()
                .find(|session| session.title == title)
            {
                return Ok((session.id.clone(), false));
            }
            let mut session = Session::new_isolated(title);
            session.trust_mode = TrustMode::FullTrust;
            let session_id = session.id.clone();
            core_state.sessions_mut().push(session);
            core_state.persist_session_and_app(&session_id)?;
            Ok((session_id, true))
        })
        .await?;
    if created {
        let _ = app.emit("sessions_updated", &());
        event_bus.publish(TiangongEvent::SessionCreated(session_id.clone()));
    }
    Ok(session_id)
}

async fn send_message_and_wait(
    app: AppHandle,
    state: &TiangongApp,
    session_id: String,
    content: String,
    message_id: Option<String>,
    media: Vec<MediaAsset>,
) -> MessageReply {
    use std::sync::mpsc as std_mpsc;
    use tiangong_types::SessionStreamEvent;

    let session_id = session_id.trim().to_string();
    if session_id.is_empty() {
        return Err("目标会话 ID 不能为空".to_string());
    }
    let message_id = message_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| scru128::new().to_string());

    state.wait_for_remote_turn_release(&session_id).await;
    let send_guard = loop {
        let session_lock = state.session_send_lock(&session_id);
        let guard = session_lock.lock_owned().await;
        if state.remote_turn_owner(&session_id).is_none() {
            break guard;
        }
        drop(guard);
        state.wait_for_remote_turn_release(&session_id).await;
    };

    let existing = state
        .with_state_read(|core_state| {
            let session = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .ok_or_else(|| anyhow!("目标会话不存在：{session_id}"))?;
            Ok(session
                .messages
                .iter()
                .find(|message| message.id == message_id)
                .map(|message| (message.role, message.turn_status)))
        })
        .await?;
    if let Some((role, turn_status)) = existing {
        if role != MessageRole::User {
            drop(send_guard);
            return Err("消息 ID 已被非用户消息占用".to_string());
        }
        if let Some(status) = turn_status {
            let result = completed_message_result(state, &session_id, &message_id, status)
                .await
                .map(|outgoing| (session_id, outgoing));
            drop(send_guard);
            return result;
        }
        let has_pending_turn = state
            .with_state_read(|core_state| Ok(core_state.has_pending_turn_for(&session_id)))
            .await?;
        drop(send_guard);
        return Err(incomplete_existing_message_error(has_pending_turn));
    }

    let has_pending_turn = state
        .with_state_read(|core_state| Ok(core_state.has_pending_turn_for(&session_id)))
        .await?;
    if has_pending_turn {
        drop(send_guard);
        return Err("目标会话已有执行中的轮次，请等待完成后重试".to_string());
    }
    state.begin_remote_turn(&session_id, &message_id)?;
    let _remote_turn_lease = RemoteTurnLease {
        state,
        session_id: session_id.clone(),
        message_id: message_id.clone(),
    };
    state.sync_core_config_from_state().await?;

    let capabilities = crate::commands::attachment_capability_snapshot(state).await?;
    let raw = media
        .into_iter()
        .map(|asset| tiangong_media_archive::RawAttachment {
            kind: asset.kind,
            source: asset.url,
            mime_type: asset.mime_type,
            original_name: asset.title,
        })
        .collect::<Vec<_>>();
    let message_id_for_prepare = message_id.clone();
    let prepared_batch = tokio::task::spawn_blocking(move || {
        let store = tiangong_media_archive::AttachmentStore::default();
        let mut transaction = store.store_batch(raw)?;
        let prepared =
            transaction.prepare_message(&message_id_for_prepare, content, capabilities)?;
        Ok::<_, String>((transaction, prepared))
    })
    .await
    .map_err(|error| format!("附件准备任务失败：{error}"))??;
    let (transaction, prepared) = prepared_batch;
    let stable_prepared = tiangong_types::stable_content_blocks(&prepared);

    let session_snapshot = state
        .with_state(|core_state| {
            let index = core_state
                .sessions()
                .iter()
                .position(|session| session.id == session_id)
                .ok_or_else(|| anyhow!("目标会话不存在：{session_id}"))?;
            let original_session = core_state.sessions()[index].clone();
            core_state.sessions_mut()[index]
                .append_prepared_user_message_with_id(message_id.clone(), stable_prepared);
            core_state.sessions_mut()[index].updated_at = tiangong_core::session::now_text();
            core_state.mark_pending_message_for(&session_id, &message_id);
            if let Err(error) = core_state.persist_session_and_app(&session_id) {
                core_state.sessions_mut()[index] = original_session;
                core_state.remove_pending_message_for(&session_id, &message_id);
                let rollback_error = core_state.persist_session_and_app(&session_id).err();
                return Err(match rollback_error {
                    Some(rollback_error) => {
                        anyhow!("消息状态持久化失败：{error}；恢复原状态也失败：{rollback_error}")
                    }
                    None => anyhow!("消息状态持久化失败：{error}"),
                });
            }
            let mut runtime_session = core_state.sessions()[index].clone();
            if runtime_session.cwd.trim().is_empty() {
                runtime_session.cwd = core_state.workspace_dir().to_string();
            }
            Ok(runtime_session)
        })
        .await?;

    let created_paths = transaction
        .newly_created_paths()
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    transaction.commit();

    let (stream_tx, stream_rx) = std_mpsc::channel::<SessionStreamEvent>();
    let ensured = state
        .ensure_core(&session_id, session_snapshot, stream_tx)
        .await;
    let waiter = state.register_remote_turn_waiter(&session_id, &message_id);
    let receipt = match state.enqueue_prepared_with_receipt_if_current(
        &ensured.session_id,
        &ensured.instance_token,
        message_id.clone(),
        prepared,
    ) {
        Ok(receipt) => receipt,
        Err(error) => {
            rollback_failed_delivery(
                state,
                &session_id,
                &message_id,
                &ensured.instance_token,
                created_paths,
            )
            .await;
            return Err(format!("消息投递失败：{error}"));
        }
    };
    if let Err(error) = receipt.await_persisted().await {
        rollback_failed_delivery(
            state,
            &session_id,
            &message_id,
            &ensured.instance_token,
            created_paths,
        )
        .await;
        return Err(format!("消息投递失败：{error}"));
    }

    if ensured.is_new {
        crate::commands::start_stream_consumer(
            app,
            ensured.session_id.clone(),
            stream_rx,
            ensured.instance_token,
        );
    }
    drop(send_guard);

    waiter
        .await
        .map_err(|_| "远程消息等待器意外关闭".to_string())?
        .map(|outgoing| (session_id, outgoing))
}

fn incomplete_existing_message_error(has_pending_turn: bool) -> String {
    if has_pending_turn {
        "该消息已存在于其他执行轮次，不能作为新的远端轮次重复投递".to_string()
    } else {
        "该消息是重启前遗留的未完成记录，当前没有可等待的执行实例".to_string()
    }
}

async fn rollback_failed_delivery(
    state: &TiangongApp,
    session_id: &str,
    message_id: &str,
    instance_token: &Arc<std::sync::atomic::AtomicBool>,
    created_paths: Vec<String>,
) {
    state.complete_remote_turn_waiters(
        session_id,
        message_id,
        Err("消息稳定持久化失败".to_string()),
    );
    crate::commands::shutdown_join_core_if_current(state, session_id, instance_token).await;
    if let Err(error) =
        crate::commands::restore_failed_user_message_state(state, session_id, message_id).await
    {
        tracing::warn!(%error, %session_id, %message_id, "内嵌 Server 消息回滚失败");
    }
    crate::commands::cleanup_unreferenced_draft_attachments(
        state,
        crate::commands::raw_attachments_for_paths(created_paths),
    )
    .await;
}

async fn completed_message_result(
    state: &TiangongApp,
    session_id: &str,
    message_id: &str,
    status: TurnStatus,
) -> HostResult<OutgoingMessage> {
    state
        .with_state_read(|core_state| {
            let session = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .ok_or_else(|| anyhow!("目标会话不存在：{session_id}"))?;
            let (outgoing, direct_agent_reply) =
                tiangong_server::remote::core::assistant_outgoing_after_user(session, message_id);
            if status == TurnStatus::Success || direct_agent_reply {
                Ok(outgoing)
            } else {
                Err(anyhow!(match status {
                    TurnStatus::Cancelled => "执行已取消",
                    TurnStatus::Failed => "执行失败",
                    TurnStatus::Success => unreachable!(),
                }))
            }
        })
        .await
}

pub(crate) async fn complete_remote_turn_from_stream(
    state: &TiangongApp,
    session_id: &str,
    message_id: &str,
) {
    let status = state
        .with_state_read(|core_state| {
            let status = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .and_then(|session| {
                    session.messages.iter().find(|message| {
                        message.id == message_id && message.role == MessageRole::User
                    })
                })
                .and_then(|message| message.turn_status)
                .ok_or_else(|| anyhow!("未找到远程消息的终态：{message_id}"))?;
            Ok(status)
        })
        .await;
    let result = match status {
        Ok(status) => completed_message_result(state, session_id, message_id, status).await,
        Err(error) => Err(error),
    };
    state.complete_remote_turn_waiters(session_id, message_id, result);
}

async fn delete_session(
    app: &AppHandle,
    state: &TiangongApp,
    session_id: &str,
) -> HostResult<bool> {
    let exists = state
        .with_state_read(|core_state| {
            Ok(core_state
                .sessions()
                .iter()
                .any(|session| session.id == session_id))
        })
        .await?;
    if !exists {
        return Ok(false);
    }

    let _draft_guard = state.draft_update_lock(session_id).lock_owned().await;
    let _send_guard = state.session_send_lock(session_id).lock_owned().await;
    crate::commands::stop_and_join_core(state, session_id).await;
    // Core 已停止且从映射取走后，任何后续删除失败都不能再依赖流 EOF 唤醒等待者。
    state.fail_remote_session_waiters(session_id, "目标会话已删除");
    let mut attachments = state
        .with_state(|core_state| {
            let mut attachments = core_state.session_input_draft(session_id).attachments;
            if let Some(session) = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
            {
                attachments.extend(crate::commands::session_attachment_candidates(session));
            }
            let active_before = core_state.active_session_id().to_string();
            core_state.delete_session_by_id(session_id)?;
            if core_state.active_session_id() != active_before {
                state.mark_active_session_changed();
            }
            Ok(attachments)
        })
        .await?;
    attachments.extend(crate::commands::raw_attachments_for_paths(
        state.release_any_draft_send_claim(session_id),
    ));
    state.remove_session_send_lock(session_id);
    tiangong_plugin_terminal::destroy_session_pty(app, session_id);
    if let Some(browser_state) = app.try_state::<tiangong_plugin_browser::BrowserPluginState>() {
        browser_state.registry.destroy_session(session_id);
    }
    if let Err(error) = crate::workspace_tabs::remove_layout(session_id) {
        tracing::warn!(%error, %session_id, "删除工作区标签页布局失败");
    }
    crate::commands::cleanup_unreferenced_draft_attachments(state, attachments).await;
    let _ = app.emit("sessions_updated", &());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bridge_declares_embedded_host_backend() {
        let (request_tx, _request_rx) = mpsc::unbounded_channel();
        let bridge = DesktopServerCoreBridge { request_tx };
        assert_eq!(bridge.kind(), CoreBackendKind::EmbeddedHost);
    }

    #[tokio::test]
    async fn stable_message_ids_keep_waiters_separate() {
        let state = TiangongApp::new();
        let mut first = state.register_remote_turn_waiter("session", "message-1");
        let second = state.register_remote_turn_waiter("session", "message-2");
        state.complete_remote_turn_waiters("session", "message-2", Err("second".to_string()));
        assert_eq!(second.await.unwrap().unwrap_err(), "second");
        assert!(first.try_recv().is_err(), "其他轮次不能被错误终态唤醒");
    }

    #[test]
    fn terminal_events_follow_each_finalized_user_message_id() {
        let mut correlation = RemoteTurnCorrelation::default();
        let final_user = |id: &str| {
            let mut message =
                tiangong_types::Message::new(tiangong_types::MessageRole::User, "request");
            message.id = id.to_string();
            message.turn_status = Some(TurnStatus::Success);
            tiangong_types::StreamEvent::SessionMessageUpsert {
                message,
                pending_plugin_deliveries: None,
                completed_plugin_delivery_ids: None,
                deferred_tool_injections: None,
            }
        };

        assert_eq!(correlation.observe(&final_user("message-1")), None);
        assert_eq!(
            correlation.observe(&tiangong_types::StreamEvent::Done { usage: None }),
            Some("message-1".to_string())
        );
        assert_eq!(correlation.observe(&final_user("message-2")), None);
        assert_eq!(
            correlation.observe(&tiangong_types::StreamEvent::Error {
                message: "failed".to_string(),
            }),
            Some("message-2".to_string())
        );
        assert_eq!(
            correlation.observe(&tiangong_types::StreamEvent::Done { usage: None }),
            None
        );
    }

    #[test]
    fn restart_leftover_message_is_rejected_instead_of_registering_a_waiter() {
        let error = incomplete_existing_message_error(false);
        assert!(error.contains("重启前遗留"));
        assert!(error.contains("没有可等待的执行实例"));
    }
}
