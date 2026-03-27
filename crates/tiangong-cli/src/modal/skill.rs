use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use tiangong_core::app_state::TiangongState;

/// 打开 Skill 管理 modal
pub fn open(state: &mut TiangongState) -> Result<()> {
    let mut selected: usize = 0;
    let mut query = String::new();
    let mut add_input: Option<String> = None;
    let mut status = "空格 启/禁用 | A 新增 | Backspace 删除 | Esc 返回".to_string();
    let mut list_state = ListState::default();

    super::run_modal(|terminal| {
        loop {
            let skills = state.installed_skills().to_vec();
            let matched: Vec<usize> = skills
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    query.is_empty() || s.id.contains(&query) || s.name.contains(&query)
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

            let is_adding = add_input.is_some();

            terminal.draw(|frame| {
                let area = frame.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Min(3),
                        Constraint::Length(3),
                        Constraint::Length(1),
                    ])
                    .split(area);

                let items: Vec<ListItem> = matched
                    .iter()
                    .enumerate()
                    .map(|(vi, &si)| {
                        let s = &skills[si];
                        let marker = if s.enabled { "●" } else { "○" };
                        let style = if vi == selected && !is_adding {
                            Style::default()
                                .bg(Color::Blue)
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };
                        let enabled_color = if s.enabled {
                            Color::Green
                        } else {
                            Color::DarkGray
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled(format!("{marker} "), Style::default().fg(enabled_color)),
                            Span::styled(format!("{}@{}", s.id, s.version), style),
                            Span::styled(
                                format!("  source={}:{}", s.source.kind, s.source.value),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]))
                    })
                    .collect();

                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title("Skill 管理"))
                    .highlight_symbol("› ");
                frame.render_stateful_widget(list, chunks[0], &mut list_state);

                let input_text = if let Some(ref input) = add_input {
                    format!("新增: {input}")
                } else {
                    format!("筛选: {query}")
                };
                let input =
                    Paragraph::new(input_text).block(Block::default().borders(Borders::ALL));
                frame.render_widget(input, chunks[1]);

                let status_line =
                    Paragraph::new(status.as_str()).style(Style::default().fg(Color::DarkGray));
                frame.render_widget(status_line, chunks[2]);
            })?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if is_adding {
                    match key.code {
                        KeyCode::Esc => {
                            add_input = None;
                            status =
                                "空格 启/禁用 | A 新增 | Backspace 删除 | Esc 返回".to_string();
                        }
                        KeyCode::Enter => {
                            if let Some(raw) = add_input.take() {
                                let path = raw.trim();
                                if path.is_empty() {
                                    status = "路径不能为空".to_string();
                                } else {
                                    match state.install_local_skill(path, true) {
                                        Ok(msg) => status = msg,
                                        Err(err) => status = format!("安装失败：{err}"),
                                    }
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(ref mut input) = add_input {
                                input.pop();
                            }
                        }
                        KeyCode::Char(ch)
                            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                        {
                            if let Some(ref mut input) = add_input {
                                input.push(ch);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Esc => break,
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        if !matched.is_empty() {
                            selected = (selected + 1).min(matched.len() - 1);
                        }
                    }
                    KeyCode::Char(' ') => {
                        if let Some(&si) = matched.get(selected) {
                            let id = skills[si].id.clone();
                            let new_enabled = !skills[si].enabled;
                            match state.set_skill_enabled(&id, new_enabled) {
                                Ok(msg) => status = msg,
                                Err(err) => status = format!("操作失败：{err}"),
                            }
                        }
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        add_input = Some(String::new());
                        status = "输入 skill 本地目录路径 | Enter 确认 | Esc 取消".to_string();
                    }
                    KeyCode::Backspace => {
                        if let Some(&si) = matched.get(selected) {
                            let id = skills[si].id.clone();
                            match state.remove_skill(&id) {
                                Ok(msg) => {
                                    status = msg;
                                    selected = selected.saturating_sub(1);
                                }
                                Err(err) => status = format!("删除失败：{err}"),
                            }
                        }
                    }
                    KeyCode::Delete => {
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
    })
}
