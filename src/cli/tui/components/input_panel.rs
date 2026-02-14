use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::super::CliApp;

impl CliApp {
    pub(in crate::cli::tui) fn render_input_panel(&self, frame: &mut ratatui::Frame, area: Rect) {
        let input = Paragraph::new(self.input.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("输入（Enter 发送，输入 / 查看命令）"),
            );
        frame.render_widget(input, area);
    }

    pub(in crate::cli::tui) fn input_cursor_position(&self, area: Rect) -> (u16, u16) {
        let max_input_width = area.width.saturating_sub(3);
        let input_char_count = self.input_cursor_prefix_display_width();
        let cursor_x = area
            .x
            .saturating_add(1)
            .saturating_add(input_char_count.min(max_input_width));
        let cursor_y = area.y.saturating_add(1);
        (cursor_x, cursor_y)
    }
}
