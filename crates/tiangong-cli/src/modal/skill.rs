use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use tiangong_app_state::app_state::TiangongState;
use tiangong_plugin_skill::SkillPlugin;

/// 打开 Skill 管理 modal
///
/// `state` 仅用于变更后触发 config 同步；skill 数据读写全部经 `skill_plugin`。
/// 删除 skill 后产生的孤儿 MCP server 经 sidecar 通道清理。
/// Skill 创建请在主对话中让 Agent 用文件工具完成（见 prompt 段落指引）。
pub fn open(_state: &mut TiangongState, skill_plugin: &Arc<SkillPlugin>) -> Result<()> {
    let mut selected: usize = 0;
    let mut query = String::new();
    let mut status = "空格 启/禁用 | Backspace 删除 | Esc 返回".to_string();
    let mut list_state = ListState::default();

    super::run_modal(|terminal| {
        loop {
            let skills = skill_plugin.installed_skills();
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
                        let style = if vi == selected {
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

                let input = Paragraph::new(format!("筛选: {query}"))
                    .block(Block::default().borders(Borders::ALL));
                frame.render_widget(input, chunks[1]);

                let status_line =
                    Paragraph::new(status.as_str()).style(Style::default().fg(Color::DarkGray));
                frame.render_widget(status_line, chunks[2]);
            })?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
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
                            let id = skills[si].id.clone();
                            let new_enabled = !skills[si].enabled;
                            match skill_plugin.set_skill_enabled(&id, new_enabled) {
                                Ok(msg) => status = msg,
                                Err(err) => status = format!("操作失败：{err}"),
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        if query.is_empty() {
                            // 在无筛选输入时，Backspace 删除选中 skill
                            if let Some(&si) = matched.get(selected) {
                                let id = skills[si].id.clone();
                                match skill_plugin.remove_skill(&id) {
                                    Ok(outcome) => {
                                        // 清理 plugin 报告的孤儿托管 MCP server
                                        for orphan in &outcome.orphan_mcp_servers {
                                            let _ = tiangong_plugin_runtime::registry::invoke_sidecar(
                                                &tiangong_config::io::storage_root(),
                                                "mcp",
                                                tiangong_plugin_mcp_protocol::management::SERVER_REMOVE_OPERATION,
                                                serde_json::to_value(
                                                    tiangong_plugin_mcp_protocol::management::RemoveServerRequest {
                                                        name: orphan.clone(),
                                                    },
                                                )
                                                .unwrap_or_default(),
                                            );
                                        }
                                        status = outcome.message;
                                        selected = selected.saturating_sub(1);
                                    }
                                    Err(err) => status = format!("删除失败：{err}"),
                                }
                            }
                        } else {
                            query.pop();
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
