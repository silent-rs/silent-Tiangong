use super::*;

/// 生成降级标题（当模型生成失败时使用）
fn generate_fallback_title(messages: &[Message]) -> String {
    use crate::core::session::MessageRole;
    use chrono::Local;

    // 尝试从对话中提取关键词
    let user_content: String = messages
        .iter()
        .filter(|msg| msg.role == MessageRole::User)
        .map(|msg| msg.content.trim())
        .collect::<Vec<_>>()
        .join(" ");

    // 如果用户输入很短，直接截取作为标题
    if user_content.len() <= 10 {
        user_content
    } else {
        // 用户输入较长，使用日期
        format!("对话 {}", Local::now().format("%m-%d"))
    }
}

impl AppTurnService {
    /// 生成会话标题（用于首次对话后自动命名，基于整个会话内容）
    fn generate_session_title(state: &TiangongState, messages: &[Message]) -> Option<String> {
        use crate::core::session::MessageRole;
        use tracing::{info, warn};

        if messages.is_empty() {
            return None;
        }

        // 构建对话摘要
        let conversation_summary: String = messages
            .iter()
            .filter(|msg| {
                // 只包含用户和助手的对话消息，排除系统消息
                matches!(msg.role, MessageRole::User | MessageRole::Assistant)
            })
            .map(|msg| {
                let role = match msg.role {
                    MessageRole::User => "用户",
                    MessageRole::Assistant => "助手",
                    _ => "",
                };
                format!("{}: {}", role, msg.content.trim())
            })
            .collect::<Vec<_>>()
            .join("\n");

        if conversation_summary.is_empty() {
            return None;
        }

        // 改进的 Prompt（更明确的要求）
        let prompt = format!(
            "你是会话标题生成助手。根据以下对话内容生成一个简洁的标题。

要求：
1. 标题长度：2-10个汉字
2. 直接返回标题文本，不要任何解释、引号或额外文字
3. 如果对话内容简单（如问候），可以用关键词作为标题
4. 标题要能概括对话的主要内容

示例：
- 对话关于Python编程 -> 'Python脚本开发'
- 对话问候 -> '新对话'
- 对话故事创作 -> '故事创作'

对话内容：
{}",
            conversation_summary
        );

        match state.services.runtime.complete_lite(&prompt) {
            Ok(title) => {
                let title = title.trim().to_string();

                // 改进的验证逻辑（更宽松）
                if title.is_empty() {
                    warn!(
                        conversation_summary = %conversation_summary[..100],
                        "轻量级模型返回空字符串，使用降级策略"
                    );
                    // 降级策略：使用默认标题
                    Some(generate_fallback_title(messages))
                } else if title.len() > 20 {
                    // 如果标题太长，截取前10个字符
                    let truncated: String = title.chars().take(10).collect();
                    info!(
                        original_title = %title,
                        truncated_title = %truncated,
                        "标题过长，已截取"
                    );
                    Some(truncated)
                } else {
                    Some(title)
                }
            }
            Err(err) => {
                warn!(
                    error = %err,
                    "轻量级模型请求失败，使用降级策略"
                );
                // 降级策略：使用默认标题
                Some(generate_fallback_title(messages))
            }
        }
    }

    pub(in crate::core::app_state) fn finish_pending_turn_success(
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

        // 在修改会话前，先收集生成标题所需的信息
        let (should_generate_title, session_messages) = state
            .store
            .session
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| {
                // 统计 user 消息数量（排除 system 和 assistant 消息）
                let user_message_count = session
                    .messages
                    .iter()
                    .filter(|msg| msg.role == MessageRole::User)
                    .count();
                (
                    // 只有当 user 消息数为 1 且标题还未生成过时才生成
                    user_message_count == 1 && !session.title_generated,
                    session.messages.clone(),
                )
            })
            .unwrap_or((false, Vec::new()));

        // 尝试生成标题（在修改会话之前）
        let generated_title = if should_generate_title {
            Self::generate_session_title(state, &session_messages)
        } else {
            None
        };

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

            // 应用生成的标题（静默后台行为）
            if let Some(new_title) = generated_title {
                session.title = new_title.clone();
                session.title_generated = true;
                session.updated_at = now_text();
                // 同步更新草稿
                state.store.session.session_title_draft = new_title;
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

    pub(in crate::core::app_state) fn finish_pending_turn_error(
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
