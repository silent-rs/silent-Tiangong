use super::super::*;

impl TiangongState {
    pub fn send_current_input(&mut self) -> Result<()> {
        let service = self.services.turn_service;
        service.send_current_input(self)
    }

    pub(in crate::app_state) fn start_turn_with_input(&mut self, input: String) -> Result<bool> {
        let service = self.services.turn_service;
        service.start_turn_with_input(self, input)
    }

    pub(in crate::app_state) fn try_recv_turn_event(
        &mut self,
        session_id: &str,
        disconnected: &mut bool,
    ) -> Option<TurnEvent> {
        let service = self.services.turn_service;
        service.try_recv_turn_event(self, session_id, disconnected)
    }

    pub(in crate::app_state) fn apply_assistant_delta(
        &mut self,
        session_id: &str,
        delta: &ModelStreamChunk,
    ) {
        let service = self.services.turn_service;
        service.apply_assistant_delta(self, session_id, delta)
    }

    pub(in crate::app_state) fn apply_stage_thinking_delta(
        &mut self,
        session_id: &str,
        stage: &str,
        delta: &ModelStreamChunk,
    ) {
        let service = self.services.turn_service;
        service.apply_stage_thinking_delta(self, session_id, stage, delta)
    }

    pub(in crate::app_state) fn append_pending_turn_llm_output(
        &mut self,
        session_id: &str,
        output: &LlmOutputRecord,
    ) {
        let service = self.services.turn_service;
        service.append_pending_turn_llm_output(self, session_id, output)
    }

    pub(in crate::app_state) fn append_pending_turn_tool_execution(
        &mut self,
        session_id: &str,
        result: &ToolResult,
    ) {
        let service = self.services.turn_service;
        service.append_pending_turn_tool_execution(self, session_id, result)
    }

    pub(in crate::app_state) fn append_pending_turn_plan_execution_summary(
        &mut self,
        session_id: &str,
        summary: &str,
    ) {
        let service = self.services.turn_service;
        service.append_pending_turn_plan_execution_summary(self, session_id, summary)
    }

    pub(in crate::app_state) fn mark_pending_turn_executing(
        &mut self,
        session_id: &str,
        plan: &TaskPlan,
    ) {
        let service = self.services.turn_service;
        service.mark_pending_turn_executing(self, session_id, plan)
    }

    pub(in crate::app_state) fn finish_pending_turn_success(
        &mut self,
        session_id: &str,
        exec: TurnExecution,
    ) {
        let service = self.services.turn_service;
        service.finish_pending_turn_success(self, session_id, exec)
    }

    pub(in crate::app_state) fn finish_pending_turn_error(
        &mut self,
        session_id: &str,
        err_msg: &str,
    ) {
        let service = self.services.turn_service;
        service.finish_pending_turn_error(self, session_id, err_msg)
    }
}
