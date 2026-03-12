use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::super::CliApp;
use crate::core::planner::PlanStepStatus;
use crate::core::runtime::RunStatus;
use crate::core::session::{SessionPlanExecutionStep, SessionTaskPlan};

impl CliApp {
    pub(in crate::cli::tui) fn render_status_panel(&self, frame: &mut ratatui::Frame, area: Rect) {
        let title = self.active_session_title_for_view().to_string();
        let plans = self.state.active_task_plans();
        let phase_tags = collect_phase_tags(
            self.state.run_snapshot().status,
            self.state.has_pending_turn(),
            !plans.is_empty(),
        );
        let progress = progress_snapshot(&plans);
        let focus = format_focus_text(&plans);
        let error_text = format_error_text(&plans);

        let active_session = self.state.active_session();
        let last_usage = self.state.run_snapshot().last_usage.clone();
        // 会话累计 = 已完成任务累计 + 当前执行中的实时消耗
        let session_usage = {
            let mut total = active_session.map(|s| s.total_usage()).unwrap_or_default();
            // 如果有正在执行的任务，其 usage 尚未写入 task_record，从 RunSnapshot 补充
            if self.state.has_pending_turn()
                && let Some(ref current) = last_usage
            {
                total.accumulate(current);
            }
            total
        };

        let mut phase_line_spans = vec![Span::styled("阶段: ", Style::default().fg(Color::Gray))];
        for (idx, tag) in phase_tags.iter().enumerate() {
            if idx > 0 {
                phase_line_spans.push(Span::styled(" + ", Style::default().fg(Color::Gray)));
            }
            phase_line_spans.push(Span::styled(
                format!("[{}]", tag.label),
                Style::default().fg(tag.color).add_modifier(Modifier::BOLD),
            ));
        }
        let progress_line = if let Some(progress) = progress {
            Line::from(vec![
                Span::styled("进度: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("plan {}/{}", progress.completed, progress.total),
                    Style::default().fg(Color::Green),
                ),
                Span::styled("  pending:", Style::default().fg(Color::Gray)),
                Span::styled(
                    progress.pending.to_string(),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("  failed:", Style::default().fg(Color::Gray)),
                Span::styled(progress.failed.to_string(), Style::default().fg(Color::Red)),
                Span::styled("  ignored:", Style::default().fg(Color::Gray)),
                Span::styled(
                    progress.ignored.to_string(),
                    Style::default().fg(Color::Magenta),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled("进度: ", Style::default().fg(Color::Gray)),
                Span::styled("无计划", Style::default().fg(Color::DarkGray)),
            ])
        };
        let header_lines = vec![
            Line::from(vec![
                Span::styled("天工 CLI", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!(
                    "  会话: {title}  模型: {}",
                    self.state.current_model()
                )),
            ]),
            Line::from(phase_line_spans),
            progress_line,
            Line::from(vec![
                Span::styled("当前: ", Style::default().fg(Color::Gray)),
                Span::styled(focus.text, Style::default().fg(focus.color)),
            ]),
            {
                let mut spans = vec![Span::styled("Token: ", Style::default().fg(Color::Gray))];
                if session_usage.total_tokens > 0 {
                    spans.push(Span::styled(
                        format!(
                            "会话 in:{} out:{} total:{}",
                            session_usage.prompt_tokens,
                            session_usage.completion_tokens,
                            session_usage.total_tokens,
                        ),
                        Style::default().fg(Color::Cyan),
                    ));
                } else {
                    spans.push(Span::styled("会话 0", Style::default().fg(Color::DarkGray)));
                }
                if let Some(ref last) = last_usage {
                    spans.push(Span::styled(
                        format!(
                            "  本轮 in:{} out:{} total:{}",
                            last.prompt_tokens, last.completion_tokens, last.total_tokens,
                        ),
                        Style::default().fg(Color::Green),
                    ));
                }
                Line::from(spans)
            },
            Line::from(Span::styled(
                self.status_message.as_str(),
                Style::default().fg(Color::Gray),
            )),
            Line::from(Span::styled(error_text, Style::default().fg(Color::Red))),
        ];
        let header = Paragraph::new(header_lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("状态"));
        frame.render_widget(header, area);
    }
}

struct PhaseTag {
    label: &'static str,
    color: Color,
}

#[derive(Debug, Clone, Copy)]
struct ProgressSnapshot {
    total: usize,
    completed: usize,
    pending: usize,
    failed: usize,
    ignored: usize,
}

struct FocusDisplay {
    text: String,
    color: Color,
}

fn collect_phase_tags(
    run_status: RunStatus,
    has_pending_turn: bool,
    has_plans: bool,
) -> Vec<PhaseTag> {
    let mut phases = Vec::new();
    if has_pending_turn {
        phases.push(PhaseTag {
            label: "running",
            color: Color::Blue,
        });
    }
    if has_pending_turn && has_plans {
        phases.push(PhaseTag {
            label: "planning",
            color: Color::Yellow,
        });
        phases.push(PhaseTag {
            label: "executing",
            color: Color::Cyan,
        });
    } else {
        phases.push(match run_status {
            RunStatus::Idle => PhaseTag {
                label: "idle",
                color: Color::DarkGray,
            },
            RunStatus::Planning => PhaseTag {
                label: "planning",
                color: Color::Yellow,
            },
            RunStatus::Executing => PhaseTag {
                label: "executing",
                color: Color::Cyan,
            },
            RunStatus::Completed => PhaseTag {
                label: "completed",
                color: Color::Green,
            },
            RunStatus::Failed => PhaseTag {
                label: "failed",
                color: Color::Red,
            },
        });
    }
    phases.dedup_by(|left, right| left.label == right.label);
    phases
}

fn progress_snapshot(plans: &[SessionTaskPlan]) -> Option<ProgressSnapshot> {
    if plans.is_empty() {
        return None;
    }
    let total = plans.len();
    let completed = plans
        .iter()
        .filter(|plan| plan.status == PlanStepStatus::Completed)
        .count();
    let failed = plans
        .iter()
        .filter(|plan| plan.status == PlanStepStatus::Failed)
        .count();
    let ignored = plans
        .iter()
        .filter(|plan| plan.status == PlanStepStatus::Ignored)
        .count();
    let pending = plans
        .iter()
        .filter(|plan| plan.status == PlanStepStatus::Pending)
        .count();
    Some(ProgressSnapshot {
        total,
        completed,
        pending,
        failed,
        ignored,
    })
}

fn format_focus_text(plans: &[SessionTaskPlan]) -> FocusDisplay {
    let Some((plan_idx, plan)) = locate_active_plan(plans) else {
        return FocusDisplay {
            text: "无".to_string(),
            color: Color::DarkGray,
        };
    };
    let step_total = plan.execution_steps.len();
    let done_steps = plan
        .execution_steps
        .iter()
        .filter(|step| step.status == PlanStepStatus::Completed)
        .count();
    if let Some((step_idx, step)) = locate_active_step(plan) {
        return FocusDisplay {
            text: format!(
                "P{}/{} {} · S{}/{} {} [{}] (done:{})",
                plan_idx + 1,
                plans.len(),
                plan.name,
                step_idx + 1,
                step_total.max(1),
                step.name,
                step_status_label(step.status),
                done_steps
            ),
            color: status_color(step.status),
        };
    }
    FocusDisplay {
        text: format!(
            "P{}/{} {} [{}] (step {}/{})",
            plan_idx + 1,
            plans.len(),
            plan.name,
            step_status_label(plan.status),
            done_steps,
            step_total
        ),
        color: status_color(plan.status),
    }
}

fn format_error_text(plans: &[SessionTaskPlan]) -> String {
    let Some((_, plan)) = locate_active_plan(plans) else {
        return String::new();
    };
    if plan.status != PlanStepStatus::Failed {
        return String::new();
    }
    let Some(summary) = plan.execution_summary.as_ref() else {
        return "失败原因：未知".to_string();
    };
    let detail = summary
        .lines()
        .find(|line| line.contains("[FAILED]"))
        .or_else(|| summary.lines().next())
        .unwrap_or("失败原因：未知");
    format!("失败: {detail}")
}

fn locate_active_plan(plans: &[SessionTaskPlan]) -> Option<(usize, &SessionTaskPlan)> {
    if plans.is_empty() {
        return None;
    }

    if let Some((idx, plan)) = plans.iter().enumerate().find(|(_, plan)| {
        plan.status == PlanStepStatus::Pending
            && plan
                .execution_summary
                .as_deref()
                .is_some_and(|summary| summary.starts_with("执行中"))
    }) {
        return Some((idx, plan));
    }

    if let Some((idx, plan)) = plans
        .iter()
        .enumerate()
        .find(|(_, plan)| plan.status == PlanStepStatus::Pending)
    {
        return Some((idx, plan));
    }

    if let Some((idx, plan)) = plans
        .iter()
        .enumerate()
        .find(|(_, plan)| plan.status == PlanStepStatus::Failed)
    {
        return Some((idx, plan));
    }

    plans
        .iter()
        .enumerate()
        .find(|(_, plan)| plan.status == PlanStepStatus::Completed)
}

fn locate_active_step(plan: &SessionTaskPlan) -> Option<(usize, &SessionPlanExecutionStep)> {
    plan.execution_steps
        .iter()
        .enumerate()
        .find(|(_, step)| step.status == PlanStepStatus::Pending)
        .or_else(|| {
            plan.execution_steps
                .iter()
                .enumerate()
                .find(|(_, step)| step.status == PlanStepStatus::Failed)
        })
}

fn step_status_label(status: PlanStepStatus) -> &'static str {
    match status {
        PlanStepStatus::Pending => "PENDING",
        PlanStepStatus::Completed => "DONE",
        PlanStepStatus::Failed => "FAILED",
        PlanStepStatus::Ignored => "IGNORED",
    }
}

fn status_color(status: PlanStepStatus) -> Color {
    match status {
        PlanStepStatus::Pending => Color::Yellow,
        PlanStepStatus::Completed => Color::Green,
        PlanStepStatus::Failed => Color::Red,
        PlanStepStatus::Ignored => Color::Magenta,
    }
}
