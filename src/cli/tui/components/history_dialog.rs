use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::super::CliApp;

impl CliApp {
    pub(in crate::cli::tui) fn render_history_dialog(&self, frame: &mut ratatui::Frame) {
        let Some(modal) = self.history_modal.as_ref() else {
            return;
        };

        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(54, 108);
        let height = area.height.saturating_sub(6).clamp(12, 22);
        let modal_rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };

        let matched = self.history_match_indices(&modal.query);
        let selected = if matched.is_empty() {
            0
        } else {
            modal.selected_idx.min(matched.len() - 1)
        };

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(2),
            ])
            .split(modal_rect);

        frame.render_widget(Clear, modal_rect);
        frame.render_widget(
            Block::default().borders(Borders::ALL).title("切换会话"),
            modal_rect,
        );

        let query_text = if modal.query.is_empty() {
            "(全部)".to_string()
        } else {
            modal.query.clone()
        };
        let query_widget = Paragraph::new(Line::from(vec![
            Span::styled("筛选: ", Style::default().fg(Color::Gray)),
            Span::styled(query_text, Style::default().fg(Color::Cyan)),
        ]))
        .block(Block::default().borders(Borders::ALL).title("关键词"));
        frame.render_widget(query_widget, sections[0]);

        let list_capacity = sections[1].height.saturating_sub(2) as usize;
        let list_start = selected.saturating_sub(list_capacity / 2);

        let lines = if matched.is_empty() {
            vec![Line::from(Span::styled(
                "没有匹配会话，继续输入关键词筛选",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            matched
                .iter()
                .skip(list_start)
                .take(list_capacity.max(1))
                .enumerate()
                .map(|(offset, session_idx)| {
                    let session = &self.state.sessions()[*session_idx];
                    let idx = *session_idx + 1;
                    let is_selected = list_start + offset == selected;
                    let is_current = session.id == self.state.active_session_id();
                    let marker = if is_selected { "› " } else { "  " };
                    let current_tag = if is_current { " · 当前" } else { "" };
                    let text = format!(
                        "{marker}{idx:>2}. {} · {}条消息{}",
                        session.title,
                        session.messages.len(),
                        current_tag
                    );
                    if is_selected {
                        Line::from(Span::styled(
                            text,
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ))
                    } else {
                        Line::from(Span::raw(text))
                    }
                })
                .collect()
        };

        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("会话列表")),
            sections[1],
        );

        let footer = Paragraph::new("↑/↓选择  Enter切换  Esc关闭")
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::ALL).title("操作"));
        frame.render_widget(footer, sections[2]);
    }
}
