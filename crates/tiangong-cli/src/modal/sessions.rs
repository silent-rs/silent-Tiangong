use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use tiangong_core::app_state::TiangongState;

/// 打开会话管理 modal，返回切换到的会话 ID（如果有）
pub fn open(state: &mut TiangongState) -> Result<Option<String>> {
    let mut selected: usize = 0;
    let mut query = String::new();
    let mut result = None;
    let mut list_state = ListState::default();

    super::run_modal(|terminal| {
        loop {
            let sessions = state.sessions();
            let matched: Vec<usize> = sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    query.is_empty() || s.title.contains(&query) || s.id.starts_with(&query)
                })
                .map(|(i, _)| i)
                .collect();

            if !matched.is_empty() {
                selected = selected.min(matched.len() - 1);
            }
            list_state.select(if matched.is_empty() {
                None
            } else {
                Some(selected)
            });

            let active_id = state.active_session_id().to_string();

            terminal.draw(|frame| {
                let area = frame.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(3), Constraint::Length(3)])
                    .split(area);

                let items: Vec<ListItem> = matched
                    .iter()
                    .enumerate()
                    .map(|(vi, &si)| {
                        let s = &sessions[si];
                        let is_active = s.id == active_id;
                        let marker = if is_active { "* " } else { "  " };
                        let style = if vi == selected {
                            Style::default()
                                .bg(Color::Blue)
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else if is_active {
                            Style::default().fg(Color::Cyan)
                        } else {
                            Style::default()
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled(marker, style),
                            Span::styled(
                                format!(
                                    "{} - {} ({} 条消息)",
                                    &s.id[..8.min(s.id.len())],
                                    s.title,
                                    s.messages.len()
                                ),
                                style,
                            ),
                        ]))
                    })
                    .collect();

                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("会话切换 (Enter 选择, Esc 返回)"),
                    )
                    .highlight_symbol("› ");
                frame.render_stateful_widget(list, chunks[0], &mut list_state);

                let input = Paragraph::new(format!("筛选: {query}"))
                    .block(Block::default().borders(Borders::ALL));
                frame.render_widget(input, chunks[1]);
            })?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Esc => break,
                    KeyCode::Enter => {
                        if let Some(&si) = matched.get(selected) {
                            result = Some(sessions[si].id.clone());
                        }
                        break;
                    }
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down if !matched.is_empty() => {
                        selected = (selected + 1).min(matched.len() - 1);
                    }
                    KeyCode::PageUp => selected = selected.saturating_sub(10),
                    KeyCode::PageDown if !matched.is_empty() => {
                        selected = (selected + 10).min(matched.len() - 1);
                    }
                    KeyCode::Home => selected = 0,
                    KeyCode::End if !matched.is_empty() => {
                        selected = matched.len() - 1;
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        selected = 0;
                    }
                    KeyCode::Char(ch) => {
                        query.push(ch);
                        selected = 0;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })?;

    if let Some(id) = &result {
        state.switch_session(id);
    }
    Ok(result)
}
