use super::*;

impl AppTurnService {
    pub(in crate::app_state) fn finish_pending_turn_success(
        self,
        state: &mut TiangongState,
        exec: TurnExecution,
    ) {
        let Some((session_id, task_id, assistant_message_id, started_at)) =
            state.store.runtime.pending_turn.as_ref().map(|pending| {
                (
                    pending.session_id.clone(),
                    pending.task_id.clone(),
                    pending.assistant_message_id.clone(),
                    pending.started_at,
                )
            })
        else {
            return;
        };

        let mut final_assistant_message_id = assistant_message_id;
        let mut updated_existing_message = false;
        if let Some(message_id) = final_assistant_message_id.as_deref()
            && let Some(message) = self.find_message_mut(state, &session_id, message_id)
        {
            message.content = exec.assistant_message.clone();
            message.reasoning_content = exec.assistant_reasoning_content.clone();
            updated_existing_message = true;
        }
        if (final_assistant_message_id.is_none() || !updated_existing_message)
            && let Some(session) = state
                .store
                .session
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
        {
            session.append_message_with_reasoning(
                MessageRole::Assistant,
                exec.assistant_message.clone(),
                exec.assistant_reasoning_content.clone(),
            );
            final_assistant_message_id = session.messages.last().map(|message| message.id.clone());
        }

        let base_result = format!(
            "success; output_mode={}; chunks={}",
            exec.output_mode, exec.output_chunk_count
        );
        let duration_ms = elapsed_ms_u64(started_at.elapsed().as_millis());
        let plan_snapshot = format_plan_snapshot(&exec.plan);
        let turn_conclusion = build_turn_conclusion(&exec);
        let tool_result_text = merge_tool_result_text(
            exec.tool_result_summary,
            exec.tool_execution.as_ref(),
            &exec.verify_records,
        );
        let result_with_workspace = match (
            workspace_change_overview(),
            summarize_verify_for_result(&exec.verify_records),
        ) {
            (Some(overview), Some(verify)) => {
                format!("{base_result}; {overview}; {verify}; {turn_conclusion}")
            }
            (Some(overview), None) => format!("{base_result}; {overview}; {turn_conclusion}"),
            (None, Some(verify)) => format!("{base_result}; {verify}; {turn_conclusion}"),
            (None, None) => format!("{base_result}; {turn_conclusion}"),
        };
        let has_failed_plan = exec
            .plan
            .plans
            .iter()
            .any(|item| item.status == PlanStepStatus::Failed);
        let completion_tool_result = tool_result_text.clone();
        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if let Some(message_id) = final_assistant_message_id.clone() {
                session.bind_task_assistant_message_id(&task_id, message_id);
            }
            session.sync_task_plans(&task_id, &exec.plan.plans);
            if has_failed_plan {
                session.fail_task_with_context_and_usage(
                    &task_id,
                    "执行失败",
                    Some("plan_failed".to_string()),
                    duration_ms,
                    Some(plan_snapshot.clone()),
                    completion_tool_result,
                    Some(exec.usage.clone()),
                );
            } else {
                session.complete_task_with_usage(
                    &task_id,
                    Some(plan_snapshot.clone()),
                    completion_tool_result,
                    duration_ms,
                    Some(exec.usage.clone()),
                );
            }
        }

        state.store.runtime.run = RunSnapshot {
            status: if has_failed_plan {
                RunStatus::Failed
            } else {
                RunStatus::Completed
            },
            summary: if has_failed_plan {
                "执行失败".to_string()
            } else {
                "执行完成".to_string()
            },
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id),
            last_duration_ms: Some(duration_ms),
            last_result: Some(if has_failed_plan {
                format!("failed; {result_with_workspace}")
            } else {
                result_with_workspace
            }),
            last_plan: Some(plan_snapshot),
            last_tool_result: tool_result_text,
            last_error: has_failed_plan.then_some("plan_failed".to_string()),
            last_usage: Some(exec.usage),
            updated_at: now_text(),
        };

        // 首次对话完成后自动生成标题（如果标题仍为默认格式）
        if let Some(session) = state
            .store
            .session
            .sessions
            .iter()
            .find(|s| s.id == session_id)
        {
            let is_default_title = session.title.starts_with("会话 ") || session.title == "默认会话";
            if is_default_title {
                let user_input_for_title = session
                    .messages
                    .iter()
                    .find(|m| m.role == MessageRole::User)
                    .map(|m| m.content.clone())
                    .unwrap_or_default();
                if !user_input_for_title.is_empty() {
                    let client = SingleProviderClient::new(
                        state.store.provider.models_config.to_chat_provider_config(),
                    );
                    if let Ok(title) = client.complete_lite(&user_input_for_title) {
                        let clean_title = title.trim().trim_matches('"').to_string();
                        if !clean_title.is_empty() {
                            if let Some(session_mut) = state
                                .store
                                .session
                                .sessions
                                .iter_mut()
                                .find(|s| s.id == session_id)
                            {
                                session_mut.title = clean_title.clone();
                                session_mut.updated_at = now_text();
                                state.store.session.session_title_draft = clean_title;
                            }
                        }
                    }
                }
            }
        }

        if let Err(err) = state.persist_session_and_app(&session_id) {
            state.store.runtime.run = RunSnapshot {
                status: RunStatus::Failed,
                summary: "会话持久化失败".to_string(),
                last_session_id: state.store.runtime.run.last_session_id.clone(),
                last_task_id: state.store.runtime.run.last_task_id.clone(),
                last_duration_ms: state.store.runtime.run.last_duration_ms,
                last_result: Some("failed".to_string()),
                last_plan: state.store.runtime.run.last_plan.clone(),
                last_tool_result: state.store.runtime.run.last_tool_result.clone(),
                last_error: Some(err.to_string()),
                last_usage: state.store.runtime.run.last_usage.clone(),
                updated_at: now_text(),
            };
        }
    }

    pub(in crate::app_state) fn finish_pending_turn_error(
        self,
        state: &mut TiangongState,
        err_msg: &str,
    ) {
        let Some((session_id, task_id, assistant_message_id, started_at)) =
            state.store.runtime.pending_turn.as_ref().map(|pending| {
                (
                    pending.session_id.clone(),
                    pending.task_id.clone(),
                    pending.assistant_message_id.clone(),
                    pending.started_at,
                )
            })
        else {
            return;
        };
        let duration_ms = elapsed_ms_u64(started_at.elapsed().as_millis());

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if let Some(assistant_message_id) = assistant_message_id.as_deref()
                && let Some(position) = session.messages.iter().position(|msg| {
                    msg.id == assistant_message_id
                        && msg.content.trim().is_empty()
                        && msg.reasoning_content.trim().is_empty()
                })
            {
                session.messages.remove(position);
                session.updated_at = now_text();
            }
            session.append_message(MessageRole::System, err_msg);
            session.fail_task(&task_id, "执行失败", Some(err_msg.to_string()), duration_ms);
        }

        state.store.runtime.run = RunSnapshot {
            status: RunStatus::Failed,
            summary: "执行失败".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id),
            last_duration_ms: Some(duration_ms),
            last_result: Some("failed".to_string()),
            last_plan: state.store.runtime.run.last_plan.clone(),
            last_tool_result: None,
            last_error: Some(err_msg.to_string()),
            last_usage: None,
            updated_at: now_text(),
        };

        if let Err(err) = state.persist_session_and_app(&session_id) {
            state.store.runtime.run = RunSnapshot {
                status: RunStatus::Failed,
                summary: "会话持久化失败".to_string(),
                last_session_id: state.store.runtime.run.last_session_id.clone(),
                last_task_id: state.store.runtime.run.last_task_id.clone(),
                last_duration_ms: state.store.runtime.run.last_duration_ms,
                last_result: Some("failed".to_string()),
                last_plan: state.store.runtime.run.last_plan.clone(),
                last_tool_result: state.store.runtime.run.last_tool_result.clone(),
                last_error: Some(err.to_string()),
                last_usage: state.store.runtime.run.last_usage.clone(),
                updated_at: now_text(),
            };
        }
    }
}
