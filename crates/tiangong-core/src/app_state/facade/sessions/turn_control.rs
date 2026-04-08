use super::super::super::*;

impl TiangongState {
    pub fn report_run_failed(&mut self, summary: impl Into<String>, error: impl Into<String>) {
        self.replace_run_snapshot(RunStatus::Failed, summary, Some(error.into()));
    }

    pub fn report_run_idle(&mut self, summary: impl Into<String>) {
        self.replace_run_snapshot(RunStatus::Idle, summary, None);
    }

    /// 响应工具审批请求（已迁移到 TiangongCore.respond_approval）
    pub fn respond_to_approval(&mut self, _request_id: &str, _approved: bool) -> Result<bool> {
        // TiangongCore 已直接处理审批，此方法保留兼容接口
        Ok(false)
    }

    /// 向正在执行的 turn 追加用户消息（已迁移到 TiangongCore.send_message）
    pub fn append_user_message_to_running_turn(&mut self, _content: &str) -> Result<bool> {
        // TiangongCore 已直接处理追加消息，此方法保留兼容接口
        Ok(false)
    }

    /// 取消当前活跃会话的 pending turn（已迁移到 TiangongCore.cancel）
    pub fn cancel_pending_turn(&mut self) -> Result<bool> {
        // TiangongCore 已直接处理取消，此方法保留兼容接口
        Ok(false)
    }

    pub fn cancel_pending_turn_for(&mut self, _session_id: &str) -> Result<bool> {
        Ok(false)
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

    /// 轮询 pending turns（已迁移到 TiangongCore 事件驱动模式）
    pub fn poll_pending_turns(&mut self) {
        // TiangongCore 直接输出 StreamEvent，不再需要 poll
    }

    pub fn poll_pending_turn_for(&mut self, _session_id: &str) {
        // TiangongCore 直接输出 StreamEvent，不再需要 poll
    }

    pub fn poll_pending_turn(&mut self) {
        // TiangongCore 直接输出 StreamEvent，不再需要 poll
    }

    pub fn poll_events(&mut self) -> Vec<StreamEvent> {
        // TiangongCore 直接输出 StreamEvent，不再需要 poll
        Vec::new()
    }
}
