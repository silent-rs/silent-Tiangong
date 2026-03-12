use super::super::super::*;

#[allow(dead_code)]
impl TiangongState {
    pub fn report_run_failed(&mut self, summary: impl Into<String>, error: impl Into<String>) {
        self.replace_run_snapshot(RunStatus::Failed, summary, Some(error.into()));
    }

    pub fn report_run_idle(&mut self, summary: impl Into<String>) {
        self.replace_run_snapshot(RunStatus::Idle, summary, None);
    }

    pub fn cancel_pending_turn(&mut self) -> Result<bool> {
        let Some(pending) = self.store.runtime.pending_turn.take() else {
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

    pub fn poll_pending_turn(&mut self) {
        let mut should_clear = false;
        let mut disconnected = false;

        while let Some(event) = self.try_recv_turn_event(&mut disconnected) {
            match event {
                TurnEvent::PlanReady(plan) => {
                    self.mark_pending_turn_executing(&plan);
                }
                TurnEvent::LlmOutput(output) => {
                    // 实时累加 token 用量到 RunSnapshot，使状态面板能展示执行中的消耗
                    if output.usage.total_tokens > 0 {
                        let run = &mut self.store.runtime.run;
                        match run.last_usage.as_mut() {
                            Some(existing) => existing.accumulate(&output.usage),
                            None => run.last_usage = Some(output.usage.clone()),
                        }
                    }
                    self.append_pending_turn_llm_output(&output);
                }
                TurnEvent::ToolExecution(result) => {
                    self.append_pending_turn_tool_execution(&result);
                }
                TurnEvent::PlanExecutionSummary(summary) => {
                    self.append_pending_turn_plan_execution_summary(summary.as_str());
                }
                TurnEvent::StageThinking { stage, delta } => {
                    self.apply_stage_thinking_delta(&stage, &delta);
                }
                TurnEvent::Chunk(delta) => {
                    self.apply_assistant_delta(&delta);
                }
                TurnEvent::Completed(exec) => {
                    self.finish_pending_turn_success(*exec);
                    should_clear = true;
                }
                TurnEvent::Failed(err_msg) => {
                    self.finish_pending_turn_error(&err_msg);
                    should_clear = true;
                }
            }
        }

        if disconnected && !should_clear {
            self.finish_pending_turn_error("执行中断：后台任务通道已关闭");
            should_clear = true;
        }

        if should_clear {
            self.store.runtime.pending_turn = None;
        }
    }
}
