use crate::planner::{PlanStepStatus, TaskPlan};
use crate::tool::ToolResult;

#[derive(Debug, Clone)]
pub struct VerifyExecutionRecord {
    pub command: String,
    pub ok: bool,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
}

pub fn build_grounded_response_prompt(
    user_input: &str,
    plan: &TaskPlan,
    tool_results: &[ToolResult],
    verify_records: &[VerifyExecutionRecord],
    mcp_context: &[String],
) -> String {
    let plan_snapshot = format_plan_snapshot(plan);
    let tool_evidence = format_tool_evidence(tool_results);
    let verify_summary = format_verify_evidence(verify_records);
    let mcp_summary = if mcp_context.is_empty() {
        "无".to_string()
    } else {
        mcp_context.join(" | ")
    };
    format!(
"你是执行结果汇总助手。请严格基于执行证据回答用户，不得编造目录、文件、命令输出或技术栈。

要求：
1. 只能使用下方证据中的事实；若证据不足，明确说未查询到或无法确认。
2. 若存在失败步骤，必须先说明失败点与影响，再给出当前可确认信息。
3. 不要把推测写成确定事实。
4. 直接回答用户问题，语言简洁。
5. 绝对不要在回复中包含工具调用的原始痕迹，如工具执行[...]、ok=、exit_code=、duration_ms=、summary:、stdout:、stderr:等元数据。只使用工具返回的业务数据来回答。

用户原始问题：
{user_input}

MCP上下文：
{mcp_summary}

计划执行快照：
{plan_snapshot}

工具执行证据：
{tool_evidence}

验证命令结果：
{verify_summary}"
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

fn format_plan_snapshot(plan: &TaskPlan) -> String {
    let mut lines = Vec::new();
    lines.push(format!("objective: {}", plan.objective));
    for (plan_idx, item) in plan.plans.iter().enumerate() {
        lines.push(format!(
            "P{} [{}] {} - {}",
            plan_idx + 1,
            plan_step_status_label(item.status),
            item.name,
            item.description
        ));
        if let Some(summary) = item.execution_summary.as_ref() {
            lines.push(format!("  RESULT: {}", summary.replace('\n', " | ")));
        }
        for (step_idx, step) in item.execution_steps.iter().enumerate() {
            lines.push(format!(
                "  S{}.{} [{}] {} - {}",
                plan_idx + 1,
                step_idx + 1,
                plan_step_status_label(step.status),
                step.name,
                step.description
            ));
        }
    }
    lines.join("\n")
}

fn format_tool_evidence(tool_results: &[ToolResult]) -> String {
    if tool_results.is_empty() {
        return "无工具执行记录".to_string();
    }

    tool_results
        .iter()
        .enumerate()
        .map(|(idx, result)| {
            let output_hint = build_output_preview(&result.stderr, &result.stdout, 12);
            format!(
                "{}. ok={} | exit_code={} | summary={} | output={}",
                idx + 1,
                result.ok,
                result.exit_code,
                result.summary,
                output_hint
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_verify_evidence(records: &[VerifyExecutionRecord]) -> String {
    if records.is_empty() {
        return "无验证命令".to_string();
    }

    records
        .iter()
        .enumerate()
        .map(|(idx, record)| {
            let output_hint = build_output_preview(&record.stderr, &record.stdout, 6);
            format!(
                "{}. ok={} | exit_code={} | cmd={} | summary={} | output={}",
                idx + 1,
                record.ok,
                record.exit_code,
                record.command,
                record.summary,
                output_hint
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_output_preview(stderr: &str, stdout: &str, max_lines: usize) -> String {
    let stderr_preview = preview_non_empty_lines(stderr, 4);
    if stderr_preview != "无可用输出" {
        return format!("stderr: {stderr_preview}");
    }
    let stdout_preview = preview_non_empty_lines(stdout, max_lines);
    format!("stdout: {stdout_preview}")
}

fn preview_non_empty_lines(text: &str, max_lines: usize) -> String {
    if max_lines == 0 {
        return "无可用输出".to_string();
    }

    const MAX_LINE_CHARS: usize = 180;
    let mut shown = Vec::new();
    let mut total = 0usize;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        total += 1;
        if shown.len() < max_lines {
            shown.push(truncate_chars(trimmed, MAX_LINE_CHARS));
        }
    }

    if shown.is_empty() {
        return "无可用输出".to_string();
    }
    if total > shown.len() {
        shown.push(format!("...(省略 {} 行)", total - shown.len()));
    }
    shown.join(" | ")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}
