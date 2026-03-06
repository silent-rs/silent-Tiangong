use super::*;

impl AppTurnService {
    pub(in crate::core::app_state) fn apply_assistant_delta(
        self,
        state: &mut TiangongState,
        delta: &ModelStreamChunk,
    ) {
        if delta.content.is_empty() && delta.reasoning_content.is_empty() {
            return;
        }

        let Some((session_id, assistant_message_id)) =
            self.ensure_pending_turn_assistant_message(state)
        else {
            return;
        };

        if let Some(message) = self.find_message_mut(state, &session_id, &assistant_message_id) {
            message.content.push_str(&delta.content);
            message.reasoning_content.push_str(&delta.reasoning_content);
        }
    }

    pub(in crate::core::app_state) fn append_pending_turn_llm_output(
        self,
        state: &mut TiangongState,
        output: &LlmOutputRecord,
    ) {
        let Some(session_id) = state
            .store
            .runtime
            .pending_turn
            .as_ref()
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.append_message(MessageRole::System, format_llm_output_message(output));
            let _ = state.persist_session_and_app(&session_id);
        }
    }

    pub(in crate::core::app_state) fn append_pending_turn_tool_execution(
        self,
        state: &mut TiangongState,
        result: &ToolResult,
    ) {
        let Some(session_id) = state
            .store
            .runtime
            .pending_turn
            .as_ref()
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.append_message(MessageRole::System, format_tool_trace_message(result));
            let _ = state.persist_session_and_app(&session_id);
        }
    }

    pub(in crate::core::app_state) fn append_pending_turn_plan_execution_summary(
        self,
        state: &mut TiangongState,
        summary: &str,
    ) {
        let Some(session_id) = state
            .store
            .runtime
            .pending_turn
            .as_ref()
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };
        let summary = summary.trim();
        if summary.is_empty() {
            return;
        }

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.append_message(MessageRole::System, format!("Plan 执行总结\n{summary}"));
            let _ = state.persist_session_and_app(&session_id);
        }
    }

    pub(in crate::core::app_state) fn ensure_pending_turn_assistant_message(
        self,
        state: &mut TiangongState,
    ) -> Option<(String, String)> {
        let (session_id, task_id, existing_message_id) =
            state.store.runtime.pending_turn.as_ref().map(|pending| {
                (
                    pending.session_id.clone(),
                    pending.task_id.clone(),
                    pending.assistant_message_id.clone(),
                )
            })?;

        if let Some(message_id) = existing_message_id {
            return Some((session_id, message_id));
        }

        let assistant_message_id = {
            let session = state
                .store
                .session
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)?;
            session.append_message(MessageRole::Assistant, String::new());
            session.messages.last().map(|msg| msg.id.clone())?
        };

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.bind_task_assistant_message_id(&task_id, assistant_message_id.clone());
        }
        if let Some(pending) = state.store.runtime.pending_turn.as_mut()
            && pending.session_id == session_id
            && pending.task_id == task_id
        {
            pending.assistant_message_id = Some(assistant_message_id.clone());
        }

        Some((session_id, assistant_message_id))
    }

    pub(in crate::core::app_state) fn mark_pending_turn_executing(
        self,
        state: &mut TiangongState,
        plan: &TaskPlan,
    ) {
        let Some((session_id, task_id)) = state
            .store
            .runtime
            .pending_turn
            .as_ref()
            .map(|pending| (pending.session_id.clone(), pending.task_id.clone()))
        else {
            return;
        };

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.mark_task_executing(&task_id, Some(format_plan_snapshot(plan)));
            session.sync_task_plans(&task_id, &plan.plans);
        }

        state.store.runtime.run = RunSnapshot {
            status: RunStatus::Executing,
            summary: "正在流式调用模型".to_string(),
            last_session_id: Some(session_id.clone()),
            last_task_id: Some(task_id),
            last_duration_ms: None,
            last_result: None,
            last_plan: Some(format_plan_snapshot(plan)),
            last_tool_result: None,
            last_error: None,
            last_usage: None,
            updated_at: now_text(),
        };

        let _ = state.persist_session_and_app(&session_id);
    }
}
