use ratatui::layout::{Constraint, Direction, Layout};

use super::CliApp;

impl CliApp {
    pub(super) fn render(&mut self, frame: &mut ratatui::Frame) {
        let area = frame.area();
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7),
                Constraint::Min(8),
                Constraint::Length(3),
            ])
            .split(area);

        self.render_status_panel(frame, sections[0]);
        let content_sections = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Ratio(3, 4), Constraint::Ratio(1, 4)])
            .split(sections[1]);
        self.conversation_panel_area = Some(content_sections[0]);
        self.plan_panel_area = Some(content_sections[1]);
        self.render_conversation_panel(frame, content_sections[0]);
        self.render_plan_panel(frame, content_sections[1]);
        self.render_input_panel(frame, sections[2]);

        let cursor = self.input_cursor_position(sections[2]);
        frame.set_cursor_position(cursor);

        if self.history_modal.is_none()
            && self.planning_modal.is_none()
            && self.mcp_modal.is_none()
            && self.skill_modal.is_none()
        {
            self.render_model_selector(frame, sections[2]);
        }
        self.render_history_dialog(frame);
        self.render_planning_dialog(frame);
        self.render_mcp_dialog(frame);
        self.render_skill_dialog(frame);
    }
}
