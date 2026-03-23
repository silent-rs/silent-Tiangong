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
        disconnected: &mut bool,
    ) -> Option<TurnEvent> {
        let service = self.services.turn_service;
        service.try_recv_turn_event(self, disconnected)
    }

    pub(in crate::app_state) fn apply_assistant_delta(&mut self, delta: &ModelStreamChunk) {
        let service = self.services.turn_service;
        service.apply_assistant_delta(self, delta)
    }

    pub(in crate::app_state) fn apply_stage_thinking_delta(
        &mut self,
        stage: &str,
        delta: &ModelStreamChunk,
    ) {
        let service = self.services.turn_service;
        service.apply_stage_thinking_delta(self, stage, delta)
    }

    pub(in crate::app_state) fn append_pending_turn_llm_output(
        &mut self,
        output: &LlmOutputRecord,
    ) {
        let service = self.services.turn_service;
        service.append_pending_turn_llm_output(self, output)
    }

    pub(in crate::app_state) fn append_pending_turn_tool_execution(&mut self, result: &ToolResult) {
        let service = self.services.turn_service;
        service.append_pending_turn_tool_execution(self, result)
    }

    pub(in crate::app_state) fn append_pending_turn_plan_execution_summary(
        &mut self,
        summary: &str,
    ) {
        let service = self.services.turn_service;
        service.append_pending_turn_plan_execution_summary(self, summary)
    }

    pub(in crate::app_state) fn mark_pending_turn_executing(&mut self, plan: &TaskPlan) {
        let service = self.services.turn_service;
        service.mark_pending_turn_executing(self, plan)
    }

    pub(in crate::app_state) fn finish_pending_turn_success(&mut self, exec: TurnExecution) {
        let service = self.services.turn_service;
        service.finish_pending_turn_success(self, exec)
    }

    pub(in crate::app_state) fn finish_pending_turn_error(&mut self, err_msg: &str) {
        let service = self.services.turn_service;
        service.finish_pending_turn_error(self, err_msg)
    }
}
