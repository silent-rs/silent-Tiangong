use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::core::session::Message;

use super::transcript::{format_transcript, text_display_width};
use super::{CliApp, MAX_COMMAND_HINTS};

impl CliApp {
    pub(super) fn render(&mut self, frame: &mut ratatui::Frame) {
        let area = frame.area();
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(8),
                Constraint::Length(3),
            ])
            .split(area);

        let title = self.active_session_title_for_view().to_string();
        let status_text = if self.state.has_pending_turn() {
            "请求中"
        } else {
            "空闲"
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
        frame.render_widget(header, sections[0]);

        let conversation_inner_width = sections[1].width.saturating_sub(2).max(1);
        let messages = self.active_messages_for_view();
        let transcript = format_transcript(&messages, conversation_inner_width);
        let logical_line_count = Paragraph::new(transcript.clone())
            .wrap(Wrap { trim: false })
            .line_count(conversation_inner_width)
            .max(1);
        let visible_rows = sections[1].height.saturating_sub(2);
        self.max_conversation_scroll = logical_line_count
            .saturating_sub(visible_rows as usize)
            .min(u16::MAX as usize) as u16;
        if self.follow_conversation_bottom
            || self.conversation_scroll > self.max_conversation_scroll
        {
            self.conversation_scroll = self.max_conversation_scroll;
        }

        let conversation = Paragraph::new(transcript)
            .wrap(Wrap { trim: false })
            .scroll((self.conversation_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title("对话"));
        frame.render_widget(conversation, sections[1]);

        let input = Paragraph::new(self.input.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("输入（Enter 发送，输入 / 查看命令）"),
            );
        frame.render_widget(input, sections[2]);

        let max_input_width = sections[2].width.saturating_sub(3);
        let input_char_count = text_display_width(self.input.as_str());
        let cursor_x = sections[2]
            .x
            .saturating_add(1)
            .saturating_add(input_char_count.min(max_input_width));
        let cursor_y = sections[2].y.saturating_add(1);
        frame.set_cursor_position((cursor_x, cursor_y));
        if self.history_modal.is_none() {
            self.render_command_palette(frame, sections[2]);
        }
        self.render_history_modal(frame);
    }

    fn render_command_palette(&self, frame: &mut ratatui::Frame, input_area: Rect) {
        let hints = self.command_hints();
        if hints.is_empty() {
            return;
        }

        let row_count = hints.len().min(MAX_COMMAND_HINTS);
        let height = row_count as u16 + 2;
        let y = input_area.y.saturating_sub(height);
        let panel = Rect {
            x: input_area.x,
            y,
            width: input_area.width,
            height,
        };

        let lines = hints
            .iter()
            .take(row_count)
            .enumerate()
            .map(|(idx, hint)| {
                let selected_idx = self.selected_hint_position(&hints);
                let command_style = if selected_idx == Some(idx) && hint.selectable {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    let color = if hint.selectable {
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    };
                    Style::default().fg(color)
                };
                let marker = if selected_idx == Some(idx) && hint.selectable {
                    "› "
                } else {
                    "  "
                };
                let desc_color = if hint.selectable {
                    Color::Gray
                } else {
                    Color::DarkGray
                };
                Line::from(vec![
                    Span::styled(format!("{marker}{:<16}", hint.command), command_style),
                    Span::styled(hint.description.as_str(), Style::default().fg(desc_color)),
                ])
            })
            .collect::<Vec<_>>();

        let widget = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("命令提示"));

        frame.render_widget(Clear, panel);
        frame.render_widget(widget, panel);
    }

    fn render_history_modal(&self, frame: &mut ratatui::Frame) {
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
            Block::default().borders(Borders::ALL).title("历史会话"),
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

    fn active_messages_for_view(&self) -> Vec<Message> {
        if self.draft_new_session {
            Vec::new()
        } else {
            self.state
                .active_session()
                .map(|session| session.messages.clone())
                .unwrap_or_default()
        }
    }

    fn active_session_title_for_view(&self) -> &str {
        if self.draft_new_session {
            "新对话"
        } else {
            self.state
                .active_session()
                .map(|session| session.title.as_str())
                .unwrap_or("默认会话")
        }
    }
}
