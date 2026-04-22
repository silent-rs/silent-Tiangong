use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use tiangong_core::app_state::TiangongState;

/// 打开 MCP 管理 modal
pub fn open(state: &mut TiangongState) -> Result<()> {
    let mut selected: usize = 0;
    let mut query = String::new();
    let mut add_input: Option<String> = None;
    let mut status = "空格 启/禁用 | A 新增 | Backspace 删除 | Esc 返回".to_string();
    let mut list_state = ListState::default();

    super::run_modal(|terminal| {
        loop {
            let servers = state.mcp_servers().to_vec();
            let matched: Vec<usize> = servers
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    query.is_empty()
                        || s.name.contains(&query)
                        || s.command.contains(&query)
                        || s.tags.iter().any(|t| t.contains(&query))
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
                        let s = &servers[si];
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
                            Span::styled(format!("{:<20}", s.name), style),
                            Span::styled(
                                format!(" {} {}", s.command, s.args.join(" ")),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]))
                    })
                    .collect();

                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("MCP Server 管理"),
                    )
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
                                match parse_and_add_mcp(state, &raw) {
                                    Ok(msg) => status = msg,
                                    Err(err) => status = format!("新增失败：{err}"),
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
                    KeyCode::Down if !matched.is_empty() => {
                        selected = (selected + 1).min(matched.len() - 1);
                    }
                    KeyCode::Char(' ') => {
                        if let Some(&si) = matched.get(selected) {
                            let name = servers[si].name.clone();
                            let new_enabled = !servers[si].enabled;
                            match state.set_mcp_server_enabled(&name, new_enabled) {
                                Ok(msg) => status = msg,
                                Err(err) => status = format!("操作失败：{err}"),
                            }
                        }
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        add_input = Some(String::new());
                        status =
                            "格式: <name> <command> [args...] | Enter 确认 | Esc 取消".to_string();
                    }
                    KeyCode::Backspace => {
                        if let Some(&si) = matched.get(selected) {
                            let name = servers[si].name.clone();
                            match state.remove_mcp_server(&name) {
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

fn parse_and_add_mcp(state: &mut TiangongState, raw: &str) -> Result<String> {
    use tiangong_core::app_state::{RegisterMcpServerOptions, RegisterMcpServerRequest};

    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(anyhow::anyhow!("至少需要 <name> <command>"));
    }
    let name = parts[0].to_string();
    let command = parts[1].to_string();
    let args: Vec<String> = parts[2..].iter().map(|s| s.to_string()).collect();

    state.register_mcp_server(RegisterMcpServerRequest {
        name,
        command,
        args,
        tags: Vec::new(),
        enabled: true,
        options: RegisterMcpServerOptions::default(),
    })
}
