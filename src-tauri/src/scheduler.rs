use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::CoreConfigProvider;
use tiangong_core::permission::TrustMode;
use tiangong_scheduler::executor::SchedulerContext;
use tokio::sync::Mutex as AsyncMutex;

/// Desktop 端调度器执行上下文
///
/// 使用 TiangongApp 共享的 state 和 config，维护独立的定时任务 core map。
/// 定时任务 core 运行在 FullTrust 模式下，与 UI 核心隔离。
pub struct DesktopSchedulerContext {
    state: Arc<AsyncMutex<tiangong_core::app_state::TiangongState>>,
    config: CoreConfigProvider,
    scheduler_cores: std::sync::Mutex<HashMap<String, TiangongCore>>,
}

impl DesktopSchedulerContext {
    pub fn new(
        state: Arc<AsyncMutex<tiangong_core::app_state::TiangongState>>,
        config: CoreConfigProvider,
    ) -> Self {
        Self {
            state,
            config,
            scheduler_cores: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl SchedulerContext for DesktopSchedulerContext {
    async fn send_message(&self, session_id: &str, content: String) -> anyhow::Result<()> {
        // 确保 core 存在
        let needs_create = {
            let cores = self.scheduler_cores.lock().unwrap();
            !cores.contains_key(session_id)
        };
        if needs_create {
            self.ensure_scheduler_core(session_id).await?;
        }

        let cores = self.scheduler_cores.lock().unwrap();
        let core = cores
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("定时任务 core 不存在：{session_id}"))?;
        if !core.send_message(content) {
            return Err(anyhow::anyhow!("定时任务 core 命令通道已关闭"));
        }
        Ok(())
    }

    async fn resolve_or_create_session(
        &self,
        requested_session_id: Option<&str>,
        trigger_name: &str,
    ) -> anyhow::Result<(String, bool)> {
        if let Some(sid) = requested_session_id {
            let state = self.state.lock().await;
            if state.sessions().iter().any(|s| s.id == *sid) {
                return Ok((sid.to_string(), false));
            }
        }

        let mut state = self.state.lock().await;
        let title = format!("定时任务：{}", trigger_name);
        let session = tiangong_core::session::Session::new_isolated(title);
        let session_id = session.id.clone();
        state.sessions_mut().push(session);
        state.persist_session(&session_id)?;
        Ok((session_id, true))
    }
}

impl DesktopSchedulerContext {
    /// 为定时任务创建 core 并启动流消费线程
    async fn ensure_scheduler_core(&self, session_id: &str) -> anyhow::Result<()> {
        let session = {
            let state = self.state.lock().await;
            state
                .sessions()
                .iter()
                .find(|s| s.id == session_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("定时任务会话不存在：{session_id}"))?
        };

        let (stream_tx, stream_rx) =
            std::sync::mpsc::channel::<tiangong_types::SessionStreamEvent>();
        let core = TiangongCore::with_session_for_gui(self.config.clone(), session, stream_tx);
        core.set_trust_mode(TrustMode::FullTrust);

        {
            let mut cores = self.scheduler_cores.lock().unwrap();
            cores.insert(session_id.to_string(), core);
        }

        // 启动后台线程消费流事件并持久化到 state
        let state = self.state.clone();
        let sid = session_id.to_string();
        std::thread::Builder::new()
            .name(format!("scheduler-stream-{sid}"))
            .spawn(move || {
                for session_event in stream_rx {
                    if let tiangong_types::StreamEvent::Done { .. }
                    | tiangong_types::StreamEvent::Error { .. } = session_event.event
                    {
                        let rt = tokio::runtime::Handle::current();
                        let state = state.clone();
                        let sid = sid.clone();
                        rt.block_on(async {
                            let mut state = state.lock().await;
                            let _ = state.persist_session(&sid);
                        });
                    }
                }
                eprintln!("定时任务流消费线程退出：{sid}");
            })?;

        Ok(())
    }
}
