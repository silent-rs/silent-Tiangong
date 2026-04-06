use super::super::super::formatting::{format_llm_output_message, format_tool_trace_message};
use super::super::super::*;

impl TiangongState {
    pub fn report_run_failed(&mut self, summary: impl Into<String>, error: impl Into<String>) {
        self.replace_run_snapshot(RunStatus::Failed, summary, Some(error.into()));
    }

    pub fn report_run_idle(&mut self, summary: impl Into<String>) {
        self.replace_run_snapshot(RunStatus::Idle, summary, None);
    }

    /// 响应工具审批请求
    pub fn respond_to_approval(&mut self, request_id: &str, approved: bool) -> Result<bool> {
        let active_id = self.store.session.active_session_id.clone();
        if let Some(pending) = self.store.runtime.pending_turns.get(&active_id) {
            let _ = pending.control_tx.send(ControlSignal::PermissionResponse {
                request_id: request_id.to_string(),
                approved,
            });
            // 立即重置审批状态，前端下次轮询即可看到变化
            self.store.runtime.run.status = RunStatus::Executing;
            self.store.runtime.run.summary = if approved {
                "审批通过，正在执行工具...".to_string()
            } else {
                "审批拒绝，正在重新决策...".to_string()
            };
            self.store.runtime.run.approval_request_id = None;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 向正在执行的 turn 追加用户消息
    pub fn append_user_message_to_running_turn(&mut self, content: &str) -> Result<bool> {
        let active_id = self.store.session.active_session_id.clone();
        if let Some(pending) = self.store.runtime.pending_turns.get(&active_id) {
            let _ = pending
                .control_tx
                .send(ControlSignal::UserMessage(content.to_string()));
            // 在 session 中记录用户追加消息
            if let Some(session) = self
                .store
                .session
                .sessions
                .iter_mut()
                .find(|s| s.id == pending.session_id)
            {
                session.append_message(MessageRole::User, content.to_string());
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn cancel_pending_turn(&mut self) -> Result<bool> {
        let active_id = self.store.session.active_session_id.clone();
        self.cancel_pending_turn_for(&active_id)
    }

    pub fn cancel_pending_turn_for(&mut self, session_id: &str) -> Result<bool> {
        let Some(pending) = self.store.runtime.pending_turns.remove(session_id) else {
            return Ok(false);
        };

        let duration_ms = elapsed_ms_u64(pending.started_at.elapsed().as_millis());
        let mut cancelled_summary = "执行已取消".to_string();
        if let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == pending.session_id)
        {
            if let Some(assistant_message_id) = pending.assistant_message_id.as_deref()
                && let Some(position) = session.messages.iter().position(|msg| {
                    msg.id == assistant_message_id
                        && msg.content.trim().is_empty()
                        && msg.reasoning_content.trim().is_empty()
                })
            {
                session.messages.remove(position);
                session.updated_at = now_text();
            }
            session.append_message(MessageRole::System, "执行已取消：用户主动中断");
            session.fail_task(
                &pending.task_id,
                "执行已取消",
                Some("cancelled_by_user".to_string()),
                duration_ms,
            );
            cancelled_summary = format!("执行已取消（会话：{}）", session.title);
        }

        self.store.runtime.run = RunSnapshot {
            status: RunStatus::Failed,
            summary: "执行已取消".to_string(),
            last_session_id: Some(pending.session_id.clone()),
            last_task_id: Some(pending.task_id),
            last_duration_ms: Some(duration_ms),
            last_result: Some("failed".to_string()),
            last_plan: self.store.runtime.run.last_plan.clone(),
            last_tool_result: self.store.runtime.run.last_tool_result.clone(),
            last_error: Some("cancelled_by_user".to_string()),
            last_usage: None,
            updated_at: now_text(),
            approval_request_id: None,
        };

        self.persist_session_and_app(&pending.session_id)?;
        self.store.runtime.run.summary = cancelled_summary;

        Ok(true)
    }

    pub fn delete_pending_task_plan(&mut self, pending_index_1_based: usize) -> Result<bool> {
        if pending_index_1_based == 0 {
            return Err(anyhow!("删除索引必须从 1 开始"));
        }

        let active_id = self.store.session.active_session_id.clone();
        let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
        else {
            return Err(anyhow!("当前会话不存在"));
        };

        let removed = session.delete_pending_task_plan(pending_index_1_based - 1);
        if removed {
            self.persist_session_and_app(&active_id)?;
        }
        Ok(removed)
    }

    pub fn move_pending_task_plan(
        &mut self,
        from_index_1_based: usize,
        to_index_1_based: usize,
    ) -> Result<bool> {
        if from_index_1_based == 0 || to_index_1_based == 0 {
            return Err(anyhow!("调序索引必须从 1 开始"));
        }

        let active_id = self.store.session.active_session_id.clone();
        let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
        else {
            return Err(anyhow!("当前会话不存在"));
        };

        let moved = session.move_pending_task_plan(from_index_1_based - 1, to_index_1_based - 1);
        if moved {
            self.persist_session_and_app(&active_id)?;
        }
        Ok(moved)
    }

    /// 轮询所有 pending_turns，处理每个会话的事件
    /// 每次最多处理 MAX_EVENTS_PER_POLL 个事件，防止长时间持锁阻塞其他操作
    pub fn poll_pending_turns(&mut self) {
        const MAX_EVENTS_PER_POLL: usize = 50;

        // 收集所有 pending session_id
        let session_ids: Vec<String> = self.store.runtime.pending_turns.keys().cloned().collect();
        let mut sessions_to_clear: Vec<String> = Vec::new();

        for session_id in session_ids {
            let mut should_clear = false;
            let mut disconnected = false;
            let mut events_processed = 0usize;

            while events_processed < MAX_EVENTS_PER_POLL {
                let Some(event) = self.try_recv_turn_event(&session_id, &mut disconnected) else {
                    break;
                };
                events_processed += 1;
                match event {
                    TurnEvent::PlanReady(plan) => {
                        self.mark_pending_turn_executing(&session_id, &plan);
                    }
                    TurnEvent::LlmOutput(output) => {
                        if output.usage.total_tokens > 0 {
                            let run = &mut self.store.runtime.run;
                            match run.last_usage.as_mut() {
                                Some(existing) => existing.accumulate(&output.usage),
                                None => run.last_usage = Some(output.usage.clone()),
                            }
                        }
                        if !output.tool_calls.is_empty() {
                            self.store.runtime.run.summary =
                                format!("正在执行：{}", output.tool_calls.join(", "));
                        }
                        self.append_pending_turn_llm_output(&session_id, &output);
                    }
                    TurnEvent::ToolStarted { name, summary } => {
                        self.store.runtime.run.summary = format!("正在执行：{name} - {summary}");
                    }
                    TurnEvent::ToolExecution(result) => {
                        self.append_pending_turn_tool_execution(&session_id, &result);
                    }
                    TurnEvent::PlanExecutionSummary(summary) => {
                        self.append_pending_turn_plan_execution_summary(
                            &session_id,
                            summary.as_str(),
                        );
                    }
                    TurnEvent::StageThinking { stage, delta } => {
                        self.apply_stage_thinking_delta(&session_id, &stage, &delta);
                    }
                    TurnEvent::WorkerStarted {
                        ref worker_id,
                        ref worker_label,
                    } => {
                        // 创建带 worker_id 的系统消息
                        self.append_worker_system_message(
                            &session_id,
                            worker_id,
                            format!("🔧 Worker: {worker_label}"),
                        );
                        self.store.runtime.run.summary = format!("Worker 开始：{worker_label}");
                    }
                    TurnEvent::WorkerEvent {
                        ref worker_id,
                        ref worker_label,
                        inner,
                    } => {
                        // 转发内部事件，创建带 worker_id 的系统消息
                        match *inner {
                            TurnEvent::LlmOutput(output) => {
                                let content = format_llm_output_message(&output);
                                self.append_worker_system_message(&session_id, worker_id, content);
                            }
                            TurnEvent::ToolExecution(result) => {
                                let content = format_tool_trace_message(&result);
                                self.append_worker_system_message(&session_id, worker_id, content);
                            }
                            TurnEvent::StageThinking { delta, .. } => {
                                // Worker 的 thinking 写入 Worker 的 assistant 消息
                                self.apply_worker_delta(
                                    &session_id,
                                    worker_id,
                                    worker_label,
                                    &delta,
                                );
                            }
                            _ => {}
                        }
                        self.store.runtime.run.summary = format!("Worker 执行中：{worker_label}");
                    }
                    TurnEvent::WorkerChunk {
                        worker_id,
                        worker_label,
                        delta,
                    } => {
                        self.apply_worker_delta(&session_id, &worker_id, &worker_label, &delta);
                    }
                    TurnEvent::WorkerCompleted { worker_label, .. } => {
                        self.store.runtime.run.summary = format!("Worker 完成：{worker_label}");
                    }
                    TurnEvent::Chunk(delta) => {
                        self.apply_assistant_delta(&session_id, &delta);
                    }
                    TurnEvent::Completed(exec) => {
                        self.finish_pending_turn_success(&session_id, *exec);
                        should_clear = true;
                    }
                    TurnEvent::Failed(err_msg) => {
                        self.finish_pending_turn_error(&session_id, &err_msg);
                        should_clear = true;
                    }
                    TurnEvent::ManagementCommand(cmd) => {
                        self.execute_management_command(cmd);
                    }
                    TurnEvent::ApprovalRequest {
                        request_id,
                        tool_name,
                        tool_args_summary,
                    } => {
                        self.store.runtime.run.status = RunStatus::WaitingApproval;
                        self.store.runtime.run.summary =
                            format!("等待审批：{tool_name}（{tool_args_summary}）");
                        self.store.runtime.run.approval_request_id = Some(request_id.clone());
                        tracing::info!(request_id, tool_name, "前端收到审批请求");
                    }
                }
            }

            if disconnected && !should_clear {
                self.finish_pending_turn_error(&session_id, "执行中断：后台任务通道已关闭");
                should_clear = true;
            }

            if should_clear {
                sessions_to_clear.push(session_id);
            }
        }

        for session_id in sessions_to_clear {
            self.store.runtime.pending_turns.remove(&session_id);
        }
    }

    /// 只轮询指定 session 的 pending turn
    pub fn poll_pending_turn_for(&mut self, session_id: &str) {
        const MAX_EVENTS_PER_POLL: usize = 50;

        if !self.store.runtime.pending_turns.contains_key(session_id) {
            return;
        }

        let mut should_clear = false;
        let mut disconnected = false;
        let mut events_processed = 0usize;

        while events_processed < MAX_EVENTS_PER_POLL {
            let Some(event) = self.try_recv_turn_event(session_id, &mut disconnected) else {
                break;
            };
            events_processed += 1;
            match event {
                TurnEvent::PlanReady(plan) => {
                    self.mark_pending_turn_executing(session_id, &plan);
                }
                TurnEvent::LlmOutput(output) => {
                    if output.usage.total_tokens > 0 {
                        let run = &mut self.store.runtime.run;
                        match run.last_usage.as_mut() {
                            Some(existing) => existing.accumulate(&output.usage),
                            None => run.last_usage = Some(output.usage.clone()),
                        }
                    }
                    // 有工具调用时更新 summary 显示即将执行的工具
                    if !output.tool_calls.is_empty() {
                        self.store.runtime.run.summary =
                            format!("正在执行：{}", output.tool_calls.join(", "));
                    }
                    self.append_pending_turn_llm_output(session_id, &output);
                }
                TurnEvent::ToolStarted { name, summary } => {
                    self.store.runtime.run.summary = format!("正在执行：{name} - {summary}");
                }
                TurnEvent::ToolExecution(result) => {
                    self.append_pending_turn_tool_execution(session_id, &result);
                }
                TurnEvent::PlanExecutionSummary(summary) => {
                    self.append_pending_turn_plan_execution_summary(session_id, summary.as_str());
                }
                TurnEvent::StageThinking { stage, delta } => {
                    self.apply_stage_thinking_delta(session_id, &stage, &delta);
                }
                TurnEvent::WorkerStarted {
                    ref worker_id,
                    ref worker_label,
                } => {
                    self.append_worker_system_message(
                        session_id,
                        worker_id,
                        format!("🔧 Worker: {worker_label}"),
                    );
                    self.store.runtime.run.summary = format!("Worker 开始：{worker_label}");
                }
                TurnEvent::WorkerEvent {
                    ref worker_id,
                    ref worker_label,
                    inner,
                } => {
                    match *inner {
                        TurnEvent::LlmOutput(output) => {
                            let content = format_llm_output_message(&output);
                            self.append_worker_system_message(session_id, worker_id, content);
                        }
                        TurnEvent::ToolExecution(result) => {
                            let content = format_tool_trace_message(&result);
                            self.append_worker_system_message(session_id, worker_id, content);
                        }
                        TurnEvent::StageThinking { delta, .. } => {
                            // Worker 的 thinking 写入 Worker 的 assistant 消息
                            self.apply_worker_delta(session_id, worker_id, worker_label, &delta);
                        }
                        _ => {}
                    }
                    self.store.runtime.run.summary = format!("Worker 执行中：{worker_label}");
                }
                TurnEvent::WorkerChunk {
                    worker_id,
                    worker_label,
                    delta,
                } => {
                    self.apply_worker_delta(session_id, &worker_id, &worker_label, &delta);
                }
                TurnEvent::WorkerCompleted {
                    ref worker_label, ..
                } => {
                    self.store.runtime.run.summary = format!("Worker 完成：{worker_label}");
                }
                TurnEvent::Chunk(delta) => {
                    self.apply_assistant_delta(session_id, &delta);
                }
                TurnEvent::Completed(exec) => {
                    self.finish_pending_turn_success(session_id, *exec);
                    should_clear = true;
                }
                TurnEvent::Failed(err_msg) => {
                    self.finish_pending_turn_error(session_id, &err_msg);
                    should_clear = true;
                }
                TurnEvent::ManagementCommand(cmd) => {
                    self.execute_management_command(cmd);
                }
                TurnEvent::ApprovalRequest {
                    request_id,
                    tool_name,
                    tool_args_summary,
                } => {
                    self.store.runtime.run.status = RunStatus::WaitingApproval;
                    self.store.runtime.run.summary =
                        format!("等待审批：{tool_name}（{tool_args_summary}）");
                    self.store.runtime.run.approval_request_id = Some(request_id);
                }
            }
        }

        if disconnected && !should_clear {
            self.finish_pending_turn_error(session_id, "执行中断：后台任务通道已关闭");
            should_clear = true;
        }

        if should_clear {
            self.store.runtime.pending_turns.remove(session_id);
        }
    }

    /// 兼容旧调用：轮询所有 pending turns
    pub fn poll_pending_turn(&mut self) {
        self.poll_pending_turns();
    }
}
