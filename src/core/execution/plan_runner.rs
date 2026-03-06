use anyhow::Result;

use crate::core::agent_config::McpConfig;
use crate::core::execution::step_executor::execute_single_plan_step_with_execution_agent;
use crate::core::execution::{DynamicPlanStep, ExecutionStepReport, LlmOutputRecord};
use crate::core::model::SingleProviderClient;
use crate::core::planner::{PlanStepSource, PlanStepStatus, TaskPlan};
use crate::core::session::{Message, Session};
use crate::core::tool::{LocalToolExecutor, ToolResult};

const MAX_DYNAMIC_STEPS_PER_PLAN: usize = 12;

#[allow(clippy::too_many_arguments)]
pub fn execute_plan_with_execution_agent<P, L, T, S>(
    client: &SingleProviderClient,
    tool_executor: &LocalToolExecutor,
    mcp_config: &McpConfig,
    plan: &mut TaskPlan,
    session: &Session,
    user_input: &str,
    context: &[Message],
    tool_results: &mut Vec<ToolResult>,
    on_llm_output: &mut L,
    on_tool_result: &mut T,
    on_plan_execution_summary: &mut S,
    on_plan_ready: &mut P,
) -> Result<()>
where
    P: FnMut(&TaskPlan),
    L: FnMut(&LlmOutputRecord),
    T: FnMut(&ToolResult),
    S: FnMut(&str),
{
    let mut previous_plan_summaries = Vec::new();

    for plan_idx in 0..plan.plans.len() {
        if plan.plans[plan_idx].status != PlanStepStatus::Pending {
            continue;
        }

        let current_plan_name = plan.plans[plan_idx].name.clone();
        let mut reports = Vec::new();
        if let Some(item) = plan.plans.get_mut(plan_idx) {
            item.execution_summary = Some("执行中：准备执行当前 plan".to_string());
        }
        on_plan_ready(plan);

        let mut step_idx = 0usize;
        while step_idx < plan.plans[plan_idx].execution_steps.len() {
            if plan.plans[plan_idx].execution_steps[step_idx].status != PlanStepStatus::Pending {
                step_idx += 1;
                continue;
            }

            let step = plan.plans[plan_idx].execution_steps[step_idx].clone();
            let tool_results_before = tool_results.len();
            let step_result = execute_single_plan_step_with_execution_agent(
                client,
                tool_executor,
                mcp_config,
                session,
                user_input,
                context,
                plan,
                &current_plan_name,
                &step,
                &previous_plan_summaries,
                tool_results,
            );
            for item in tool_results.iter().skip(tool_results_before) {
                on_tool_result(item);
            }
            match step_result {
                Ok(step_result) => {
                    if let Some(output) = step_result.llm_output {
                        let record = LlmOutputRecord {
                            stage: format!("execution-agent::{current_plan_name}::{}", step.name),
                            content: output.content,
                            reasoning_content: output.reasoning_content,
                            tool_calls: output.tool_calls,
                        };
                        on_llm_output(&record);
                    }
                    mark_step_status(plan, plan_idx, step_idx, PlanStepStatus::Completed);
                    reports.push(step_result.report.clone());
                    if let Some(next_step) = step_result.next_step {
                        if plan.plans[plan_idx].execution_steps.len() >= MAX_DYNAMIC_STEPS_PER_PLAN
                        {
                            mark_step_status(plan, plan_idx, step_idx, PlanStepStatus::Failed);
                            reports.push(ExecutionStepReport {
                                step_name: step.name.clone(),
                                status: PlanStepStatus::Failed,
                                summary: format!(
                                    "动态步骤超过上限（max={}），中止当前 plan",
                                    MAX_DYNAMIC_STEPS_PER_PLAN
                                ),
                            });
                            let ignored =
                                mark_remaining_steps_ignored(plan, plan_idx, step_idx + 1);
                            for ignored_step in ignored {
                                reports.push(ExecutionStepReport {
                                    step_name: ignored_step.name,
                                    status: PlanStepStatus::Ignored,
                                    summary: "前置步骤失败，本步骤已忽略".to_string(),
                                });
                            }
                            break;
                        }
                        append_dynamic_step(plan, plan_idx, next_step);
                    }
                }
                Err(err) => {
                    mark_step_status(plan, plan_idx, step_idx, PlanStepStatus::Failed);
                    reports.push(ExecutionStepReport {
                        step_name: step.name.clone(),
                        status: PlanStepStatus::Failed,
                        summary: err.to_string(),
                    });
                    let ignored = mark_remaining_steps_ignored(plan, plan_idx, step_idx + 1);
                    for ignored_step in ignored {
                        reports.push(ExecutionStepReport {
                            step_name: ignored_step.name,
                            status: PlanStepStatus::Ignored,
                            summary: "前置步骤失败，本步骤已忽略".to_string(),
                        });
                    }
                    break;
                }
            }
            on_plan_ready(plan);
            step_idx += 1;
        }

        if let Some(item) = plan.plans.get_mut(plan_idx) {
            let summary = summarize_plan_execution(item.name.as_str(), &reports);
            item.execution_summary = Some(summary.clone());
            item.refresh_status();
            on_plan_execution_summary(summary.as_str());
            previous_plan_summaries.push(summary);
        }
        plan.refresh_plan_statuses();
        on_plan_ready(plan);
    }
    Ok(())
}

pub fn summarize_tool_results(results: &[ToolResult]) -> Option<String> {
    if results.is_empty() {
        return None;
    }
    Some(
        results
            .iter()
            .map(format_tool_result_for_display)
            .collect::<Vec<_>>()
            .join("；"),
    )
}

fn format_tool_result_for_display(result: &ToolResult) -> String {
    if let Some(record) = &result.execution {
        format!(
            "{} | tool={} | exit_code={} | duration={}ms",
            result.summary, record.tool_name, record.exit_code, record.duration_ms
        )
    } else {
        result.summary.clone()
    }
}

fn append_dynamic_step(plan: &mut TaskPlan, plan_idx: usize, next_step: DynamicPlanStep) {
    let Some(item) = plan.plans.get_mut(plan_idx) else {
        return;
    };
    item.execution_steps.push(crate::core::planner::PlanStep {
        id: scru128::new().to_string(),
        name: next_step.name,
        description: next_step.description,
        status: PlanStepStatus::Pending,
        source: PlanStepSource::Dynamic,
    });
}

#[derive(Debug, Clone)]
struct IgnoredStep {
    name: String,
}

fn mark_step_status(plan: &mut TaskPlan, plan_idx: usize, step_idx: usize, status: PlanStepStatus) {
    if let Some(item) = plan.plans.get_mut(plan_idx) {
        if let Some(step) = item.execution_steps.get_mut(step_idx) {
            step.status = status;
        }
        item.refresh_status();
    }
    plan.refresh_plan_statuses();
}

fn mark_remaining_steps_ignored(
    plan: &mut TaskPlan,
    plan_idx: usize,
    from_step_idx: usize,
) -> Vec<IgnoredStep> {
    let mut ignored = Vec::new();
    if let Some(item) = plan.plans.get_mut(plan_idx) {
        for step in item.execution_steps.iter_mut().skip(from_step_idx) {
            if step.status == PlanStepStatus::Pending {
                step.status = PlanStepStatus::Ignored;
                ignored.push(IgnoredStep {
                    name: step.name.clone(),
                });
            }
        }
        item.refresh_status();
    }
    plan.refresh_plan_statuses();
    ignored
}

fn summarize_plan_execution(plan_name: &str, reports: &[ExecutionStepReport]) -> String {
    if reports.is_empty() {
        return format!("{plan_name}: 未执行任何步骤");
    }

    let mut done = 0usize;
    let mut failed = 0usize;
    let mut ignored = 0usize;
    let mut lines = Vec::new();
    for report in reports {
        match report.status {
            PlanStepStatus::Completed => done += 1,
            PlanStepStatus::Failed => failed += 1,
            PlanStepStatus::Ignored => ignored += 1,
            PlanStepStatus::Pending => {}
        }
        lines.push(format!(
            "- [{}] {} => {}",
            plan_step_status_label(report.status),
            report.step_name,
            report.summary
        ));
    }
    format!(
        "{plan_name}: completed={done}, failed={failed}, ignored={ignored}\n{}",
        lines.join("\n")
    )
}

fn plan_step_status_label(status: PlanStepStatus) -> &'static str {
    match status {
        PlanStepStatus::Pending => "PENDING",
        PlanStepStatus::Completed => "DONE",
        PlanStepStatus::Failed => "FAILED",
        PlanStepStatus::Ignored => "IGNORED",
    }
}
