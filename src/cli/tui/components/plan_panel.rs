use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::core::planner::PlanStepStatus;

use super::super::CliApp;

impl CliApp {
    pub(in crate::cli::tui) fn render_plan_panel(
        &mut self,
        frame: &mut ratatui::Frame,
        area: Rect,
    ) {
        let all_plans = if self.draft_new_session {
            Vec::new()
        } else {
            self.state.active_task_plans()
        };
        let plans = if let Some(task_id) = self.state.run.last_task_id.as_ref() {
            let filtered = all_plans
                .iter()
                .filter(|plan| plan.task_id == *task_id)
                .cloned()
                .collect::<Vec<_>>();
            if filtered.is_empty() {
                all_plans
            } else {
                filtered
            }
        } else {
            all_plans
        };

        let mut lines = Vec::new();
        if plans.is_empty() {
            lines.push(Line::from(Span::styled(
                "当前会话暂无 plan",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                "输入任务后会自动生成",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (plan_idx, plan) in plans.iter().enumerate() {
                let title_style = status_style(plan.status).add_modifier(Modifier::BOLD);
                lines.push(Line::from(Span::styled(
                    format!(
                        "P{} [{}] {}",
                        plan_idx + 1,
                        status_label(plan.status),
                        plan.name
                    ),
                    title_style,
                )));
                lines.push(Line::from(Span::styled(
                    format!("   {}", plan.description),
                    Style::default().fg(Color::Gray),
                )));

                if plan.execution_steps.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "   - 无执行步骤",
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    for (step_idx, step) in plan.execution_steps.iter().enumerate() {
                        lines.push(Line::from(Span::styled(
                            format!(
                                "   S{}.{} [{}] {}",
                                plan_idx + 1,
                                step_idx + 1,
                                status_label(step.status),
                                step.name
                            ),
                            status_style(step.status),
                        )));
                    }
                }

                if let Some(summary) = plan.execution_summary.as_ref() {
                    lines.push(Line::from(Span::styled(
                        format!("   => {}", single_line(summary)),
                        Style::default().fg(Color::Cyan),
                    )));
                }
                lines.push(Line::default());
            }
        }

        let text = Text::from(lines);
        let panel_inner_width = area.width.saturating_sub(2).max(1);
        let logical_line_count = Paragraph::new(text.clone())
            .wrap(Wrap { trim: false })
            .line_count(panel_inner_width)
            .max(1);
        let visible_rows = area.height.saturating_sub(2);
        self.max_plan_scroll = logical_line_count
            .saturating_sub(visible_rows as usize)
            .min(u16::MAX as usize) as u16;
        if self.plan_scroll > self.max_plan_scroll {
            self.plan_scroll = self.max_plan_scroll;
        }

        let panel = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((self.plan_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title("计划（Plan）"));
        frame.render_widget(panel, area);
    }
}

fn status_label(status: PlanStepStatus) -> &'static str {
    match status {
        PlanStepStatus::Pending => "PENDING",
        PlanStepStatus::Completed => "DONE",
        PlanStepStatus::Failed => "FAILED",
        PlanStepStatus::Ignored => "IGNORED",
    }
}

fn status_style(status: PlanStepStatus) -> Style {
    match status {
        PlanStepStatus::Pending => Style::default().fg(Color::Yellow),
        PlanStepStatus::Completed => Style::default().fg(Color::Green),
        PlanStepStatus::Failed => Style::default().fg(Color::Red),
        PlanStepStatus::Ignored => Style::default().fg(Color::Magenta),
    }
}

fn single_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("无")
        .to_string()
}
