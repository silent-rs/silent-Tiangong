use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::core::planner::PlanStepStatus;

use super::super::CliApp;

impl CliApp {
    pub(in crate::cli::tui) fn render_planning_dialog(&self, frame: &mut ratatui::Frame) {
        let Some(modal) = self.planning_modal.as_ref() else {
            return;
        };

        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(56, 116);
        let height = area.height.saturating_sub(6).clamp(14, 28);
        let modal_rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };

        let plans = self.state.active_task_plans();
        let selected = if plans.is_empty() {
            0
        } else {
            modal.selected_idx.min(plans.len() - 1)
        };

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(modal_rect);

        frame.render_widget(Clear, modal_rect);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title("planning 列表"),
            modal_rect,
        );

        let total = plans.len();
        let pending_count = plans
            .iter()
            .filter(|plan| plan.status == PlanStepStatus::Pending)
            .count();
        let completed_count = total.saturating_sub(pending_count);
        let summary_widget = Paragraph::new(Line::from(vec![
            Span::styled("总数: ", Style::default().fg(Color::Gray)),
            Span::styled(
                total.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  pending: ", Style::default().fg(Color::Gray)),
            Span::styled(
                pending_count.to_string(),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("  completed: ", Style::default().fg(Color::Gray)),
            Span::styled(
                completed_count.to_string(),
                Style::default().fg(Color::Green),
            ),
        ]))
        .block(Block::default().borders(Borders::ALL).title("概览"));
        frame.render_widget(summary_widget, sections[0]);

        let list_capacity = sections[1].height.saturating_sub(2) as usize;
        let list_start = selected.saturating_sub(list_capacity / 2);

        let mut pending_index = 0usize;
        let pending_indexes = plans
            .iter()
            .map(|plan| {
                if plan.status == PlanStepStatus::Pending {
                    pending_index += 1;
                    Some(pending_index)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let plan_lines = if plans.is_empty() {
            vec![Line::from(Span::styled(
                "当前会话没有 plan 事项",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            plans
                .iter()
                .enumerate()
                .skip(list_start)
                .take(list_capacity.max(1))
                .enumerate()
                .map(|(offset, (idx, plan))| {
                    let is_selected = list_start + offset == selected;
                    let marker = if is_selected { "› " } else { "  " };
                    let prefix = if let Some(p_idx) = pending_indexes[idx] {
                        format!("[P{p_idx}]")
                    } else {
                        "[DONE]".to_string()
                    };
                    let total_steps = plan.execution_steps.len();
                    let done_steps = plan
                        .execution_steps
                        .iter()
                        .filter(|step| step.status == PlanStepStatus::Completed)
                        .count();
                    let text = format!(
                        "{marker}{:<8} {} · {} ({}/{})",
                        prefix, plan.name, plan.description, done_steps, total_steps
                    );

                    match (is_selected, plan.status) {
                        (true, PlanStepStatus::Pending) => Line::from(Span::styled(
                            text,
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        )),
                        (true, PlanStepStatus::Completed) => Line::from(Span::styled(
                            text,
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Gray)
                                .add_modifier(Modifier::CROSSED_OUT),
                        )),
                        (false, PlanStepStatus::Pending) => {
                            Line::from(Span::styled(text, Style::default().fg(Color::White)))
                        }
                        (false, PlanStepStatus::Completed) => Line::from(Span::styled(
                            text,
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::CROSSED_OUT),
                        )),
                    }
                })
                .collect()
        };

        frame.render_widget(
            Paragraph::new(plan_lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("plan 事项")),
            sections[1],
        );

        let step_lines = if let Some(plan) = plans.get(selected) {
            if plan.execution_steps.is_empty() {
                vec![Line::from(Span::styled(
                    "该 plan 暂无执行步骤",
                    Style::default().fg(Color::DarkGray),
                ))]
            } else {
                plan.execution_steps
                    .iter()
                    .enumerate()
                    .map(|(idx, step)| {
                        let prefix = if step.status == PlanStepStatus::Completed {
                            format!("[DONE:{}]", idx + 1)
                        } else {
                            format!("[TODO:{}]", idx + 1)
                        };
                        let text = format!("{:<10} {} · {}", prefix, step.name, step.description);
                        if step.status == PlanStepStatus::Completed {
                            Line::from(Span::styled(
                                text,
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::CROSSED_OUT),
                            ))
                        } else {
                            Line::from(Span::styled(text, Style::default().fg(Color::White)))
                        }
                    })
                    .collect()
            }
        } else {
            vec![Line::from(Span::styled(
                "请选择 plan 查看执行步骤",
                Style::default().fg(Color::DarkGray),
            ))]
        };
        frame.render_widget(
            Paragraph::new(step_lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("执行步骤")),
            sections[2],
        );

        let footer = Paragraph::new("↑/↓选择  D删除pending plan  K上移  J下移  Esc关闭")
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::ALL).title("操作"));
        frame.render_widget(footer, sections[3]);
    }
}
