use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::super::CliApp;
use crate::core::runtime::RunStatus;

impl CliApp {
    pub(in crate::cli::tui) fn render_status_panel(&self, frame: &mut ratatui::Frame, area: Rect) {
        let title = self.active_session_title_for_view().to_string();
        let status_text = match self.state.run.status {
            RunStatus::Idle => "空闲",
            RunStatus::Planning => "planning",
            RunStatus::Executing => "executing",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
        };
        let header_lines = vec![
            Line::from(vec![
                Span::styled("天工 CLI", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!(
                    "  会话: {title}  模型: {}  状态: {status_text}",
                    self.state.current_model()
                )),
            ]),
            Line::from(Span::styled(
                self.status_message.as_str(),
                Style::default().fg(Color::Gray),
            )),
        ];
        let header = Paragraph::new(header_lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("状态"));
        frame.render_widget(header, area);
    }
}
