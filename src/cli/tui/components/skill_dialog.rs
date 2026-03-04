use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::super::CliApp;

impl CliApp {
    pub(in crate::cli::tui) fn render_skill_dialog(&self, frame: &mut ratatui::Frame) {
        let Some(modal) = self.skill_modal.as_ref() else {
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

        let matched = self.skill_match_indices(&modal.query);
        let selected = if matched.is_empty() {
            0
        } else {
            modal.selected_idx.min(matched.len() - 1)
        };

        let mut constraints = vec![
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Min(4),
        ];
        if modal.add_input.is_some() {
            constraints.push(Constraint::Length(4));
        } else {
            constraints.push(Constraint::Length(2));
        }

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(modal_rect);

        frame.render_widget(Clear, modal_rect);
        frame.render_widget(
            Block::default().borders(Borders::ALL).title("Skill 管理"),
            modal_rect,
        );

        let total = self.state.installed_skills().len();
        let enabled = self
            .state
            .installed_skills()
            .iter()
            .filter(|skill| skill.enabled)
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

        let list_capacity = sections[1].height.saturating_sub(2) as usize;
        let list_start = selected.saturating_sub(list_capacity / 2);

        let lines = if matched.is_empty() {
            vec![Line::from(Span::styled(
                "没有匹配 skill，输入关键词筛选或按 A 新增",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            matched
                .iter()
                .skip(list_start)
                .take(list_capacity.max(1))
                .enumerate()
                .map(|(offset, skill_idx)| {
                    let skill = &self.state.installed_skills()[*skill_idx];
                    let is_selected = list_start + offset == selected;
                    let marker = if is_selected { "› " } else { "  " };
                    let status = if skill.enabled { "enabled" } else { "disabled" };
                    let text = format!(
                        "{marker}{:>2}. {} · {} · {}",
                        skill_idx + 1,
                        skill.id,
                        skill.version,
                        status,
                    );
                    if is_selected {
                        Line::from(Span::styled(
                            text,
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ))
                    } else if skill.enabled {
                        Line::from(Span::styled(text, Style::default().fg(Color::White)))
                    } else {
                        Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
                    }
                })
                .collect::<Vec<_>>()
        };

        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Skill 列表")),
            sections[1],
        );

        let detail_lines = if let Some(skill_idx) = matched.get(selected) {
            let skill = &self.state.installed_skills()[*skill_idx];
            vec![
                Line::from(vec![
                    Span::styled("id: ", Style::default().fg(Color::Gray)),
                    Span::styled(skill.id.clone(), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("name: ", Style::default().fg(Color::Gray)),
                    Span::styled(skill.name.clone(), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("version: ", Style::default().fg(Color::Gray)),
                    Span::styled(skill.version.clone(), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("enabled: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        skill.enabled.to_string(),
                        if skill.enabled {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default().fg(Color::Red)
                        },
                    ),
                ]),
                Line::from(vec![
                    Span::styled("source: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        format!("{}:{}", skill.source.kind, skill.source.value),
                        Style::default().fg(Color::Cyan),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("entry: ", Style::default().fg(Color::Gray)),
                    Span::styled(skill.entry.clone(), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("requires_mcp: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        skill.requires_mcp.len().to_string(),
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("managed_mcp: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        if skill.managed_mcp_servers.is_empty() {
                            "(none)".to_string()
                        } else {
                            skill.managed_mcp_servers.join(",")
                        },
                        Style::default().fg(Color::White),
                    ),
                ]),
            ]
        } else {
            vec![Line::from(Span::styled(
                "请选择 skill 查看详情",
                Style::default().fg(Color::DarkGray),
            ))]
        };

        frame.render_widget(
            Paragraph::new(detail_lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("详情")),
            sections[2],
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
                sections[3],
            );
        } else {
            frame.render_widget(
                Paragraph::new(
                    "↑/↓选择  Space启/禁用  Backspace删除  Delete删筛选字  A新增  Esc关闭",
                )
                .style(Style::default().fg(Color::Gray))
                .block(Block::default().borders(Borders::ALL).title("操作")),
                sections[3],
            );
        }
    }
}
