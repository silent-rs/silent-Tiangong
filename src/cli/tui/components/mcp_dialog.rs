use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::super::CliApp;

impl CliApp {
    pub(in crate::cli::tui) fn render_mcp_dialog(&self, frame: &mut ratatui::Frame) {
        let Some(modal) = self.mcp_modal.as_ref() else {
            return;
        };

        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(62, 124);
        let height = area.height.saturating_sub(6).clamp(16, 30);
        let modal_rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };

        let matched = self.mcp_match_indices(&modal.query);
        let selected = if matched.is_empty() {
            0
        } else {
            modal.selected_idx.min(matched.len() - 1)
        };

        let mut constraints = vec![Constraint::Length(3), Constraint::Min(10)];
        if modal.add_input.is_some() {
            constraints.push(Constraint::Length(4));
        } else {
            constraints.push(Constraint::Length(2));
        }

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(modal_rect);
        let content_sections = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
            .split(sections[1]);

        frame.render_widget(Clear, modal_rect);
        frame.render_widget(
            Block::default().borders(Borders::ALL).title("MCP 管理"),
            modal_rect,
        );

        let total = self.state.mcp_servers().len();
        let enabled = self
            .state
            .mcp_servers()
            .iter()
            .filter(|server| server.enabled)
            .count();
        let overview = Paragraph::new(Line::from(vec![
            Span::styled("筛选: ", Style::default().fg(Color::Gray)),
            Span::styled(
                if modal.query.is_empty() {
                    "(全部)".to_string()
                } else {
                    modal.query.clone()
                },
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  total: ", Style::default().fg(Color::Gray)),
            Span::styled(total.to_string(), Style::default().fg(Color::White)),
            Span::styled("  enabled: ", Style::default().fg(Color::Gray)),
            Span::styled(enabled.to_string(), Style::default().fg(Color::Green)),
            Span::styled("  matched: ", Style::default().fg(Color::Gray)),
            Span::styled(
                matched.len().to_string(),
                Style::default().fg(Color::Yellow),
            ),
        ]))
        .block(Block::default().borders(Borders::ALL).title("概览"));
        frame.render_widget(overview, sections[0]);

        let list_capacity = content_sections[0].height.saturating_sub(2) as usize;
        let list_start = selected.saturating_sub(list_capacity / 2);

        let lines = if matched.is_empty() {
            vec![Line::from(Span::styled(
                "没有匹配 MCP server，输入关键词筛选或按 A 新增",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            matched
                .iter()
                .skip(list_start)
                .take(list_capacity.max(1))
                .enumerate()
                .map(|(offset, server_idx)| {
                    let server = &self.state.mcp_servers()[*server_idx];
                    let is_selected = list_start + offset == selected;
                    let marker = if is_selected { "› " } else { "  " };
                    let status = if server.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    };
                    let transport = match server.resolved_transport() {
                        crate::core::agent_config::ResolvedMcpTransport::Stdio => "stdio",
                        crate::core::agent_config::ResolvedMcpTransport::Http => "http",
                        crate::core::agent_config::ResolvedMcpTransport::Metadata => "metadata",
                    };
                    let text = format!(
                        "{marker}{:>2}. {} · {} · {} · tags={}",
                        server_idx + 1,
                        server.name,
                        transport,
                        status,
                        if server.tags.is_empty() {
                            "(none)".to_string()
                        } else {
                            server.tags.join(",")
                        }
                    );
                    if is_selected {
                        Line::from(Span::styled(
                            text,
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ))
                    } else if server.enabled {
                        Line::from(Span::styled(text, Style::default().fg(Color::White)))
                    } else {
                        Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
                    }
                })
                .collect::<Vec<_>>()
        };

        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("MCP server 列表"),
            ),
            content_sections[0],
        );

        let detail_lines = if let Some(server_idx) = matched.get(selected) {
            let server = &self.state.mcp_servers()[*server_idx];
            let cached_tools = self.state.mcp_server_cached_tools(&server.name);
            let (tools_text, tools_style) = match cached_tools {
                Some(tools) if tools.is_empty() => {
                    ("(none)".to_string(), Style::default().fg(Color::DarkGray))
                }
                Some(tools) => {
                    let mut text = tools
                        .iter()
                        .take(6)
                        .map(|tool| tool.compact_signature())
                        .collect::<Vec<_>>()
                        .join("; ");
                    if tools.len() > 6 {
                        text.push_str(&format!(" ...(+{})", tools.len() - 6));
                    }
                    (text, Style::default().fg(Color::White))
                }
                None => (
                    "(loading, wait a moment)".to_string(),
                    Style::default().fg(Color::Yellow),
                ),
            };
            vec![
                Line::from(vec![
                    Span::styled("name: ", Style::default().fg(Color::Gray)),
                    Span::styled(server.name.clone(), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("enabled: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        server.enabled.to_string(),
                        if server.enabled {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default().fg(Color::Red)
                        },
                    ),
                ]),
                Line::from(vec![
                    Span::styled("command: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        if server.command_text().is_empty() {
                            "(empty)".to_string()
                        } else {
                            server.command.clone()
                        },
                        Style::default().fg(Color::Cyan),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("transport: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        match server.resolved_transport() {
                            crate::core::agent_config::ResolvedMcpTransport::Stdio => "stdio",
                            crate::core::agent_config::ResolvedMcpTransport::Http => "http",
                            crate::core::agent_config::ResolvedMcpTransport::Metadata => "metadata",
                        },
                        Style::default().fg(Color::Yellow),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("endpoint: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        server
                            .resolved_http_endpoint()
                            .unwrap_or("(none)")
                            .to_string(),
                        Style::default().fg(Color::Cyan),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("args: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        if server.args.is_empty() {
                            "(none)".to_string()
                        } else {
                            server.args.join(" ")
                        },
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("auth: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        if server.auth_header.trim().is_empty() {
                            "(none)".to_string()
                        } else {
                            "(set)".to_string()
                        },
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("headers: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        server.headers.len().to_string(),
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("env: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        server.env.len().to_string(),
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("cwd: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        server.cwd_text().unwrap_or("(none)").to_string(),
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("tags: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        if server.tags.is_empty() {
                            "(none)".to_string()
                        } else {
                            server.tags.join(",")
                        },
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("tools: ", Style::default().fg(Color::Gray)),
                    Span::styled(tools_text, tools_style),
                ]),
            ]
        } else {
            vec![Line::from(Span::styled(
                "请选择 MCP server 查看详情",
                Style::default().fg(Color::DarkGray),
            ))]
        };

        frame.render_widget(
            Paragraph::new(detail_lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("详情")),
            content_sections[1],
        );

        if let Some(add_input) = modal.add_input.as_ref() {
            frame.render_widget(
                Paragraph::new(add_input.clone())
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("新增（Enter提交 Esc取消）"),
                    ),
                sections[2],
            );
        } else {
            frame.render_widget(
                Paragraph::new(
                    "↑/↓选择  Space启/禁用  Backspace删除  Delete删筛选字  A新增  Esc关闭",
                )
                .style(Style::default().fg(Color::Gray))
                .block(Block::default().borders(Borders::ALL).title("操作")),
                sections[2],
            );
        }
    }
}
