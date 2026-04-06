use super::*;

impl AppTurnService {
    pub(in crate::app_state) fn send_current_input(self, state: &mut TiangongState) -> Result<()> {
        let input = state.store.session.input_draft.trim().to_string();
        let started = self.start_turn_with_input(state, input)?;
        if started {
            state.store.session.input_draft.clear();
        }
        Ok(())
    }

    pub(in crate::app_state) fn start_turn_with_input(
        self,
        state: &mut TiangongState,
        input: String,
    ) -> Result<bool> {
        if input.trim().is_empty() {
            return Ok(false);
        }
        let active_idx = state.ensure_active_session_index();
        let session_id_check = state.store.session.sessions[active_idx].id.clone();
        if state
            .store
            .runtime
            .pending_turns
            .contains_key(&session_id_check)
        {
            return Ok(false);
        }
        let session_id = state.store.session.sessions[active_idx].id.clone();

        // 首次发消息时，立即用用户输入截断作为临时标题（LLM 完成后会精炼）
        {
            let session = &mut state.store.session.sessions[active_idx];
            let is_default_title = session.title == "新对话"
                || session.title.starts_with("会话 ")
                || session.title == "默认会话";
            if is_default_title && session.messages.is_empty() {
                let trimmed_input = input.trim();
                let temp_title: String = trimmed_input.chars().take(30).collect();
                let temp_title = if trimmed_input.chars().count() > 30 {
                    format!("{}...", temp_title)
                } else {
                    temp_title
                };
                if !temp_title.is_empty() {
                    session.title = temp_title.clone();
                    session.updated_at = now_text();
                    state.store.session.session_title_draft = temp_title;
                }
            }
        }

        let task_id = new_scru128_string();
        state.store.session.sessions[active_idx].append_message(MessageRole::User, input.clone());
        let user_message_id = state.store.session.sessions[active_idx]
            .messages
            .last()
            .map(|msg| msg.id.clone())
            .ok_or_else(|| anyhow!("创建用户消息失败"))?;
        state.store.session.sessions[active_idx].start_task(
            task_id.clone(),
            user_message_id,
            String::new(),
            input.clone(),
        );

        state.store.runtime.run = RunSnapshot {
            status: RunStatus::Executing,
            summary: "正在处理".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id.clone()),
            last_duration_ms: None,
            last_result: None,
            last_plan: None,
            last_tool_result: None,
            last_error: None,
            last_usage: None,
            updated_at: now_text(),
            approval_request_id: None,
        };

        // 在创建 snapshot 前，检查并更新滚动摘要
        // 摘要持久化到 session，原始 messages 保持完整
        {
            let context_limit = state.services.runtime.context_limit;
            let organizer = crate::context::organizer::ContextOrganizer::new(context_limit)
                .with_keep_recent_turns(6);
            let session = &state.store.session.sessions[active_idx];
            if organizer.needs_compression_estimated(session) {
                let client = SingleProviderClient::new(
                    state.store.provider.models_config.to_chat_provider_config(),
                );
                let session_mut = &mut state.store.session.sessions[active_idx];
                match organizer.maybe_update_summary(session_mut, &client) {
                    Ok(true) => tracing::info!("滚动摘要已更新"),
                    Ok(false) => {}
                    Err(err) => tracing::warn!("滚动摘要更新失败：{err}"),
                }
            }
        }

        state.persist_session_and_app(&session_id)?;

        let runtime = state.services.runtime.clone();
        let session_snapshot = state.store.session.sessions[active_idx].clone();
        let (tx, rx) = mpsc::channel::<TurnEvent>();
        let (event_tx, event_rx) = mpsc::channel::<crate::event_loop::LoopEvent>();

        // 用 EventLoopRunner 替代 TurnRunner/TaskCoordinator
        let output_tx = tx.clone();
        thread::spawn(move || {
            let runner = crate::event_loop::EventLoopRunner::new(
                runtime,
                session_snapshot,
                output_tx.clone(),
                event_rx,
            );

            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runner.run()));

            match outcome {
                Ok(crate::event_loop::LoopOutcome::Suspended(_)) => {
                    // 正常挂起 — 发送 Completed 让 poll 知道本轮结束
                    let _ = output_tx.send(TurnEvent::Completed(Box::new(
                        crate::runtime::TurnExecution {
                            assistant_message: String::new(),
                            assistant_reasoning_content: String::new(),
                            system_prompt: String::new(),
                            plan: Default::default(),
                            tool_result_summary: None,
                            tool_execution: None,
                            verify_records: Vec::new(),
                            output_mode: "event_loop".to_string(),
                            output_chunk_count: 0,
                            usage: Default::default(),
                            llm_calls: Vec::new(),
                        },
                    )));
                }
                Ok(crate::event_loop::LoopOutcome::Error(_, err)) => {
                    let _ = output_tx.send(TurnEvent::Failed(err));
                }
                Ok(crate::event_loop::LoopOutcome::Cancelled(_)) => {
                    let _ = output_tx.send(TurnEvent::Failed("执行已取消".to_string()));
                }
                Ok(crate::event_loop::LoopOutcome::Shutdown(_)) => {
                    // 关闭信号，静默结束
                }
                Err(panic_err) => {
                    let reason = if let Some(s) = panic_err.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic_err.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "未知原因".to_string()
                    };
                    let _ = output_tx.send(TurnEvent::Failed(format!("内部错误：{reason}")));
                }
            }
        });

        // 向 EventLoopRunner 发送初始用户消息事件
        let _ = event_tx.send(crate::event_loop::LoopEvent::UserMessage { content: input });

        // 将 ctrl_tx 包装为兼容层（ControlSignal → LoopEvent 转发）
        let (ctrl_tx, ctrl_rx_compat) = mpsc::channel::<ControlSignal>();
        {
            let event_tx_for_compat = event_tx.clone();
            thread::spawn(move || {
                while let Ok(signal) = ctrl_rx_compat.recv() {
                    let loop_event = match signal {
                        ControlSignal::Cancel => crate::event_loop::LoopEvent::Cancel,
                        ControlSignal::UserMessage(msg) => {
                            crate::event_loop::LoopEvent::UserMessage { content: msg }
                        }
                        ControlSignal::PermissionResponse {
                            request_id,
                            approved,
                        } => crate::event_loop::LoopEvent::PermissionResponse {
                            request_id,
                            approved,
                        },
                    };
                    if event_tx_for_compat.send(loop_event).is_err() {
                        break;
                    }
                }
            });
        }

        state.store.runtime.pending_turns.insert(
            session_id.clone(),
            PendingTurn {
                session_id,
                task_id,
                assistant_message_id: None,
                stage_thinking_message_id: None,
                worker_message_ids: std::collections::HashMap::new(),
                started_at: Instant::now(),
                rx,
                control_tx: ctrl_tx,
            },
        );

        Ok(true)
    }

    pub(in crate::app_state) fn try_recv_turn_event(
        self,
        state: &mut TiangongState,
        session_id: &str,
        disconnected: &mut bool,
    ) -> Option<TurnEvent> {
        let pending = state.store.runtime.pending_turns.get(session_id)?;

        match pending.rx.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                *disconnected = true;
                None
            }
        }
    }
}
