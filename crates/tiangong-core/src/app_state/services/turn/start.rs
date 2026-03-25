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
        };

        state.persist_session_and_app(&session_id)?;

        let runtime = state.services.runtime.clone();
        let session_snapshot = state.store.session.sessions[active_idx].clone();
        let worker_input = input.clone();
        let (tx, rx) = mpsc::channel::<TurnEvent>();

        thread::spawn(move || {
            let chunk_tx = tx.clone();
            let plan_tx = tx.clone();
            let llm_tx = tx.clone();
            let tool_tx = tx.clone();
            let plan_summary_tx = tx.clone();
            let stage_thinking_tx = tx.clone();
            let result = runtime.execute_turn_with_streaming(
                &session_snapshot,
                &worker_input,
                |plan| {
                    let _ = plan_tx.send(TurnEvent::PlanReady(plan.clone()));
                },
                |delta| {
                    let _ = chunk_tx.send(TurnEvent::Chunk(delta.clone()));
                },
                |output| {
                    let _ = llm_tx.send(TurnEvent::LlmOutput(output.clone()));
                },
                |tool_result| {
                    let _ = tool_tx.send(TurnEvent::ToolExecution(tool_result.clone()));
                },
                |summary| {
                    let _ =
                        plan_summary_tx.send(TurnEvent::PlanExecutionSummary(summary.to_string()));
                },
                |stage: &str, delta: &ModelStreamChunk| {
                    let _ = stage_thinking_tx.send(TurnEvent::StageThinking {
                        stage: stage.to_string(),
                        delta: delta.clone(),
                    });
                },
            );

            match result {
                Ok(exec) => {
                    let _ = tx.send(TurnEvent::Completed(Box::new(exec)));
                }
                Err(err) => {
                    let _ = tx.send(TurnEvent::Failed(RuntimeEngine::fallback_error_message(
                        &err,
                    )));
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
                started_at: Instant::now(),
                rx,
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
