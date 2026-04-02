use super::*;
use crate::runtime::strip_tool_traces_from_response;

impl AppTurnService {
    pub(in crate::app_state) fn apply_assistant_delta(
        self,
        state: &mut TiangongState,
        session_id: &str,
        delta: &ModelStreamChunk,
    ) {
        if delta.content.is_empty() && delta.reasoning_content.is_empty() {
            return;
        }

        let Some((sid, assistant_message_id)) =
            self.ensure_pending_turn_assistant_message(state, session_id)
        else {
            return;
        };

        if let Some(message) = self.find_message_mut(state, &sid, &assistant_message_id) {
            message.content.push_str(&delta.content);
            message.reasoning_content.push_str(&delta.reasoning_content);

            // 仅当内容包含工具 trace 特征时才清理，避免破坏正常 Markdown 格式
            if message.content.contains("工具执行") && message.content.contains("ok=") {
                let cleaned = strip_tool_traces_from_response(&message.content);
                if cleaned.len() != message.content.len() {
                    message.content = cleaned;
                }
            }
        }
    }

    /// 将 stage thinking 流式 delta 追加到对应 stage 的系统消息中。
    /// 首个 chunk 会创建系统消息并记录其 ID，后续 chunk 直接追加。
    pub(in crate::app_state) fn apply_stage_thinking_delta(
        self,
        state: &mut TiangongState,
        session_id: &str,
        stage: &str,
        delta: &ModelStreamChunk,
    ) {
        if delta.content.is_empty() && delta.reasoning_content.is_empty() {
            return;
        }

        let Some(sid) = state
            .store
            .runtime
            .pending_turns
            .get(session_id)
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };

        // 检查是否已有该 stage 的系统消息
        let existing_msg_id = state
            .store
            .runtime
            .pending_turns
            .get(session_id)
            .and_then(|pending| pending.stage_thinking_message_id.clone());

        if let Some(msg_id) = existing_msg_id {
            // 追加到已有消息
            if let Some(session) = state
                .store
                .session
                .sessions
                .iter_mut()
                .find(|session| session.id == sid)
                && let Some(message) = session.messages.iter_mut().find(|m| m.id == msg_id)
            {
                message.reasoning_content.push_str(&delta.reasoning_content);
                message.content.push_str(&delta.content);
            }
        } else {
            // 创建新的系统消息，以 LLM 输出格式开头，方便 transcript 渲染识别
            // 末尾换行确保 header 独占一行，后续 delta.content 追加后不会破坏 parse_llm_event_markdown 的解析
            let initial_content = format!("LLM 输出 [{}]\n", stage);
            if let Some(session) = state
                .store
                .session
                .sessions
                .iter_mut()
                .find(|session| session.id == sid)
            {
                session.append_message(MessageRole::System, initial_content);
                if let Some(new_msg) = session.messages.last_mut() {
                    new_msg.reasoning_content.push_str(&delta.reasoning_content);
                    new_msg.content.push_str(&delta.content);
                    let new_msg_id = new_msg.id.clone();
                    if let Some(pending) = state.store.runtime.pending_turns.get_mut(session_id) {
                        pending.stage_thinking_message_id = Some(new_msg_id);
                    }
                }
            }
        }
    }

    pub(in crate::app_state) fn append_pending_turn_llm_output(
        self,
        state: &mut TiangongState,
        session_id: &str,
        output: &LlmOutputRecord,
    ) {
        let Some(sid) = state
            .store
            .runtime
            .pending_turns
            .get(session_id)
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };

        // 获取并清除 stage_thinking_message_id
        let stage_msg_id = state
            .store
            .runtime
            .pending_turns
            .get_mut(session_id)
            .and_then(|pending| pending.stage_thinking_message_id.take());

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == sid)
        {
            if let Some(msg_id) = stage_msg_id {
                // 已有 stage thinking 系统消息，更新为最终格式化内容
                if let Some(message) = session.messages.iter_mut().find(|m| m.id == msg_id) {
                    message.content = format_llm_output_message(output);
                    // reasoning_content 保留在独立字段供 TUI 渲染，不混入 content 避免重复提交给 LLM
                    message.reasoning_content = output.reasoning_content.clone();
                }
            } else {
                // 没有 stage thinking 消息（可能是非流式模式），创建新的系统消息
                session.append_message_with_reasoning(
                    MessageRole::System,
                    format_llm_output_message(output),
                    output.reasoning_content.clone(),
                );
            }
            // 不在中间事件持久化，减少文件 I/O，完成时统一持久化
        }
    }

    pub(in crate::app_state) fn append_pending_turn_tool_execution(
        self,
        state: &mut TiangongState,
        session_id: &str,
        result: &ToolResult,
    ) {
        let Some(sid) = state
            .store
            .runtime
            .pending_turns
            .get(session_id)
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == sid)
        {
            session.append_message(MessageRole::System, format_tool_trace_message(result));
            // 不在中间事件持久化，减少文件 I/O
        }
    }

    pub(in crate::app_state) fn append_pending_turn_plan_execution_summary(
        self,
        state: &mut TiangongState,
        session_id: &str,
        summary: &str,
    ) {
        let Some(sid) = state
            .store
            .runtime
            .pending_turns
            .get(session_id)
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
            .find(|session| session.id == sid)
        {
            session.append_message(MessageRole::System, format!("Plan 执行总结\n{summary}"));
        }
    }

    pub(in crate::app_state) fn ensure_pending_turn_assistant_message(
        self,
        state: &mut TiangongState,
        session_id: &str,
    ) -> Option<(String, String)> {
        let (sid, task_id, existing_message_id) =
            state.store.runtime.pending_turns.get(session_id).map(|pending| {
                (
                    pending.session_id.clone(),
                    pending.task_id.clone(),
                    pending.assistant_message_id.clone(),
                )
            })?;

        if let Some(message_id) = existing_message_id {
            return Some((sid, message_id));
        }

        let assistant_message_id = {
            let session = state
                .store
                .session
                .sessions
                .iter_mut()
                .find(|session| session.id == sid)?;
            session.append_message(MessageRole::Assistant, String::new());
            session.messages.last().map(|msg| msg.id.clone())?
        };

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == sid)
        {
            session.bind_task_assistant_message_id(&task_id, assistant_message_id.clone());
        }
        if let Some(pending) = state.store.runtime.pending_turns.get_mut(session_id)
            && pending.session_id == sid
            && pending.task_id == task_id
        {
            pending.assistant_message_id = Some(assistant_message_id.clone());
        }

        Some((sid, assistant_message_id))
    }

    pub(in crate::app_state) fn mark_pending_turn_executing(
        self,
        state: &mut TiangongState,
        session_id: &str,
        plan: &TaskPlan,
    ) {
        let Some((sid, task_id, assistant_message_id)) =
            state.store.runtime.pending_turns.get(session_id).map(|pending| {
                (
                    pending.session_id.clone(),
                    pending.task_id.clone(),
                    pending.assistant_message_id.clone(),
                )
            })
        else {
            return;
        };

        // 清除 stage thinking 消息 ID，执行阶段会创建新的 stage 消息
        if let Some(pending) = state.store.runtime.pending_turns.get_mut(session_id) {
            pending.stage_thinking_message_id = None;
        }

        if let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == sid)
        {
            // 规划阶段的流式输出已写入系统消息（而非 assistant 消息），
            // 进入执行阶段后清空 assistant 消息（如果有残留），避免与最终响应内容混在一起。
            if let Some(msg_id) = assistant_message_id.as_deref()
                && let Some(message) = session.messages.iter_mut().find(|m| m.id == msg_id)
            {
                message.content.clear();
                message.reasoning_content.clear();
            }
            session.mark_task_executing(&task_id, Some(format_plan_snapshot(plan)));
            session.sync_task_plans(&task_id, &plan.plans);
        }

        state.store.runtime.run = RunSnapshot {
            status: RunStatus::Executing,
            summary: "正在流式调用模型".to_string(),
            last_session_id: Some(sid.clone()),
            last_task_id: Some(task_id),
            last_duration_ms: None,
            last_result: None,
            last_plan: Some(format_plan_snapshot(plan)),
            last_tool_result: None,
            last_error: None,
            last_usage: None,
            updated_at: now_text(),
            approval_request_id: None,
        };

        let _ = state.persist_session_and_app(&sid);
    }

    /// 为指定 Worker 创建或追加独立的 assistant 消息
    pub(in crate::app_state) fn apply_worker_delta(
        self,
        state: &mut TiangongState,
        session_id: &str,
        _worker_id: &str,
        _worker_label: &str,
        delta: &ModelStreamChunk,
    ) {
        if delta.content.is_empty() && delta.reasoning_content.is_empty() {
            return;
        }

        let Some(sid) = state
            .store
            .runtime
            .pending_turns
            .get(session_id)
            .map(|pending| pending.session_id.clone())
        else {
            return;
        };

        let Some(session) = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == sid)
        else {
            return;
        };

        // 查找最后一条 assistant 消息（如果存在且在当前 Worker 的系统消息之后）
        // 通过检查最后一条消息是否是 assistant 来判断
        let last_is_assistant = session.messages.last()
            .is_some_and(|m| m.role == MessageRole::Assistant);

        if last_is_assistant {
            // 追加到当前 Worker 的 assistant 消息
            if let Some(msg) = session.messages.last_mut() {
                msg.content.push_str(&delta.content);
                msg.reasoning_content.push_str(&delta.reasoning_content);
            }
        } else {
            // 创建新的 assistant 消息（当前 Worker 的第一个 Chunk）
            session.append_message_with_reasoning(
                MessageRole::Assistant,
                delta.content.clone(),
                delta.reasoning_content.clone(),
            );
        }

        state.store.runtime.run.summary = format!("Worker 执行中：{_worker_label}");
    }
}
