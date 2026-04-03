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
        let worker_input = input.clone();
        let (tx, rx) = mpsc::channel::<TurnEvent>();
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<ControlSignal>();

        thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // 通过 TaskCoordinator 执行（自动判断单/多 Worker）
                let coordinator = crate::coordinator::TaskCoordinator::new(runtime.clone());
                let task = crate::coordinator::CoordinatorTask {
                    id: scru128::new().to_string(),
                    objective: worker_input.clone(),
                    user_input: worker_input,
                    context: Vec::new(),
                };
                let result =
                    coordinator.coordinate(task, &session_snapshot, Some(&tx), Some(ctrl_rx))?;

                // 将 CoordinatorResult 转为 TurnExecution
                Ok(crate::runtime::TurnExecution {
                    assistant_message: result.final_response,
                    assistant_reasoning_content: String::new(),
                    system_prompt: String::new(),
                    plan: Default::default(),
                    tool_result_summary: if result.worker_results.len() > 1 {
                        Some(format!(
                            "{} 个 Worker 并行执行，{} 成功",
                            result.worker_results.len(),
                            result.worker_results.iter().filter(|r| r.success).count()
                        ))
                    } else {
                        None
                    },
                    tool_execution: None,
                    verify_records: Vec::new(),
                    output_mode: "stream".to_string(),
                    output_chunk_count: 0,
                    usage: result.total_usage,
                    llm_calls: result
                        .worker_results
                        .into_iter()
                        .flat_map(|w| w.llm_calls)
                        .collect(),
                })
            }));

            match outcome {
                Ok(Ok(exec)) => {
                    let _ = tx.send(TurnEvent::Completed(Box::new(exec)));
                }
                Ok(Err(err)) => {
                    let msg = RuntimeEngine::fallback_error_message(&err);
                    let _ = tx.send(TurnEvent::Failed(msg));
                }
                Err(panic_err) => {
                    let reason = if let Some(s) = panic_err.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic_err.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "未知原因".to_string()
                    };
                    let _ = tx.send(TurnEvent::Failed(format!("内部错误：{reason}")));
                }
            }
        });

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
