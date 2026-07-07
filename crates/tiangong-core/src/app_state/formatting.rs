#![allow(dead_code)]

use super::*;

pub(super) fn merge_tool_result_text(
    base: Option<String>,
    record: Option<&ToolExecutionRecord>,
    verify_records: &[VerifyExecutionRecord],
) -> Option<String> {
    let base_text = match (base, record) {
        (Some(base), Some(record)) => Some(format!(
            "{base} | args={} | ok={}",
            record.args.join(" "),
            record.ok
        )),
        (Some(base), None) => Some(base),
        (None, Some(record)) => Some(format!(
            "{} | args={} | ok={}",
            record.summary,
            record.args.join(" "),
            record.ok
        )),
        (None, None) => None,
    };

    let verify_text = summarize_verify_for_result(verify_records);
    match (base_text, verify_text) {
        (Some(base), Some(verify)) => Some(format!("{base} | {verify}")),
        (Some(base), None) => Some(base),
        (None, Some(verify)) => Some(verify),
        (None, None) => None,
    }
}

pub(super) fn format_plan_snapshot(plan: &TaskPlan) -> String {
    let risks = if plan.risks.is_empty() {
        "无".to_string()
    } else {
        plan.risks.join("；")
    };
    let plan_count = plan.plans.len();
    let step_count = plan
        .plans
        .iter()
        .map(|item| item.execution_steps.len())
        .sum::<usize>();
    let capability_hints = if plan.capability_hints.is_empty() {
        "无".to_string()
    } else {
        plan.capability_hints.join("；")
    };
    let revisions = if plan.revisions.is_empty() {
        "无".to_string()
    } else {
        plan.revisions
            .iter()
            .enumerate()
            .map(|(idx, revision)| {
                format!(
                    "{}. [{}] {} => {}",
                    idx + 1,
                    revision.phase,
                    revision.reason,
                    revision.summary_after_revision
                )
            })
            .collect::<Vec<_>>()
            .join("；")
    };

    format!(
        "{}\n目标：{}\n事项数：{}\n执行步骤数：{}\n风险：{}\n能力提示：{}\n计划修正：{}",
        plan.summary, plan.objective, plan_count, step_count, risks, capability_hints, revisions
    )
}

pub(crate) fn format_llm_output_message(output: &LlmOutputRecord) -> String {
    let mut lines = vec![format!("LLM 输出 [{}]", output.stage)];
    if output.usage.total_tokens > 0 {
        lines.push(format!(
            "tokens: prompt={}, completion={}, total={}",
            output.usage.prompt_tokens, output.usage.completion_tokens, output.usage.total_tokens
        ));
    }
    if !output.tool_calls.is_empty() {
        lines.push(format!("tool_calls: {}", output.tool_calls.join(", ")));
    }
    // reasoning_content 不写入系统消息的 content，避免作为上下文重复提交给 LLM
    // thinking 内容仅通过 message.reasoning_content 字段保留用于 TUI 展示
    if !output.content.trim().is_empty() {
        lines.push(format!("content:\n{}", output.content.trim()));
    }
    lines.join("\n")
}

pub(crate) fn format_tool_trace_message(result: &ToolResult) -> String {
    let Some(record) = result.execution.as_ref() else {
        let mut lines = vec!["工具执行 [unknown]".to_string()];
        lines.push(format!("summary: {}", result.summary));
        if !result.stdout.trim().is_empty() {
            lines.push("stdout:".to_string());
            lines.push("```text".to_string());
            lines.push(result.stdout.clone());
            lines.push("```".to_string());
        }
        if !result.stderr.trim().is_empty() {
            lines.push("stderr:".to_string());
            lines.push("```text".to_string());
            lines.push(result.stderr.clone());
            lines.push("```".to_string());
        }
        return lines.join("\n");
    };

    let mut lines = vec![format!("工具执行 [{}]", record.tool_name)];
    if let Some(command) = format_tool_command(record) {
        lines.push(format!("命令: {command}"));
    }
    lines.push(format!(
        "ok={} exit_code={} duration_ms={}",
        result.ok, result.exit_code, record.duration_ms
    ));
    lines.push(format!("summary: {}", result.summary));
    if !result.stdout.trim().is_empty() {
        lines.push("stdout:".to_string());
        lines.push("```text".to_string());
        lines.push(result.stdout.clone());
        lines.push("```".to_string());
    }
    if !result.stderr.trim().is_empty() {
        lines.push("stderr:".to_string());
        lines.push("```text".to_string());
        lines.push(result.stderr.clone());
        lines.push("```".to_string());
    }
    lines.join("\n")
}

fn format_tool_command(record: &ToolExecutionRecord) -> Option<String> {
    let args = record
        .args
        .iter()
        .filter(|arg| !arg.starts_with("__tiangong_cwd="))
        .cloned()
        .collect::<Vec<_>>();
    if args.is_empty() {
        return None;
    }

    if record.tool_name == "run_command" {
        if args.first().map(String::as_str) == Some("__tiangong_shell__") {
            let script = args.get(1).cloned().unwrap_or_default();
            let shell = args.get(2).cloned().unwrap_or_else(|| "auto".to_string());
            return Some(format!("shell={shell} script={script}"));
        }
        let cmd = args.first().cloned().unwrap_or_default();
        let rest = args.into_iter().skip(1).collect::<Vec<_>>();
        if rest.is_empty() {
            return Some(cmd);
        }
        return Some(format!("{cmd} {}", rest.join(" ")));
    }

    if record.tool_name == "write_file" {
        let path = args.first().cloned().unwrap_or_default();
        let content_bytes = args.get(1).map(|content| content.len()).unwrap_or(0usize);
        let append = args.get(2).cloned().unwrap_or_else(|| "false".to_string());
        return Some(format!(
            "path={} content=...({content_bytes} bytes) append={append}",
            single_line_ellipsis(path.as_str(), 120)
        ));
    }

    Some(args.join(" "))
}

fn single_line_ellipsis(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return String::new();
    }
    let mut chars = normalized.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

pub(super) fn workspace_change_overview() -> Option<String> {
    let mut status_command = Command::new("git");
    tiangong_types::process::configure_no_window(status_command.arg("status").arg("--short"));
    let status_output = status_command.output().ok()?;
    if !status_output.status.success() {
        return None;
    }
    let status_text = String::from_utf8_lossy(&status_output.stdout);
    let mut files = Vec::new();
    for line in status_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = trimmed
            .split_whitespace()
            .last()
            .unwrap_or(trimmed)
            .to_string();
        files.push(path);
    }
    if files.is_empty() {
        return Some("changed_files=0".to_string());
    }

    let preview_limit = 6usize;
    let preview = files
        .iter()
        .take(preview_limit)
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    let extra = files.len().saturating_sub(preview_limit);
    let file_part = if extra == 0 {
        format!("changed_files={},files={preview}", files.len())
    } else {
        format!(
            "changed_files={},files={}...(+{})",
            files.len(),
            preview,
            extra
        )
    };

    let mut diff_command = Command::new("git");
    tiangong_types::process::configure_no_window(diff_command.arg("diff").arg("--stat"));
    let diff_output = diff_command.output().ok()?;
    if !diff_output.status.success() {
        return Some(file_part);
    }
    let diff_text = String::from_utf8_lossy(&diff_output.stdout);
    let mut stat_summary = String::new();
    for line in diff_text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.contains("files changed")
            || trimmed.contains("file changed")
            || trimmed.contains("insertions")
            || trimmed.contains("deletions")
        {
            stat_summary = trimmed.to_string();
            break;
        }
    }

    if stat_summary.is_empty() {
        Some(file_part)
    } else {
        Some(format!("{file_part}; diff={stat_summary}"))
    }
}

pub(super) fn build_turn_conclusion(exec: &TurnExecution) -> String {
    let mut completed = vec!["计划生成".to_string(), "模型响应".to_string()];
    if let Some(tool_execution) = &exec.tool_execution {
        completed.push(format!("工具执行({})", tool_execution.tool_name));
    }
    let pending_plans = exec
        .plan
        .plans
        .iter()
        .filter(|item| item.status == PlanStepStatus::Pending)
        .map(|item| item.name.clone())
        .collect::<Vec<_>>();
    let failed_plans = exec
        .plan
        .plans
        .iter()
        .filter(|item| item.status == PlanStepStatus::Failed)
        .map(|item| {
            let summary = item.execution_summary.clone().unwrap_or_default();
            if summary.is_empty() {
                item.name.clone()
            } else {
                format!("{}({})", item.name, summary.replace('\n', " | "))
            }
        })
        .collect::<Vec<_>>();
    let ignored_step_count = exec
        .plan
        .plans
        .iter()
        .flat_map(|item| item.execution_steps.iter())
        .filter(|step| step.status == PlanStepStatus::Ignored)
        .count();
    let failed_verify = exec
        .verify_records
        .iter()
        .filter(|record| !record.ok)
        .collect::<Vec<_>>();

    if pending_plans.is_empty() && failed_plans.is_empty() {
        completed.push("plan事项执行".to_string());
    }

    let pending = if !pending_plans.is_empty() {
        format!("待完成 plan：{}", pending_plans.join("；"))
    } else if !failed_plans.is_empty() {
        format!(
            "plan执行存在失败：{}；忽略步骤数={ignored_step_count}",
            failed_plans.join("；")
        )
    } else if exec.verify_records.is_empty() {
        "人工复核输出结果".to_string()
    } else if failed_verify.is_empty() {
        completed.push("验证执行".to_string());
        "无".to_string()
    } else {
        let hints = failed_verify
            .iter()
            .map(|record| format!("{} => {}", record.command, record.summary))
            .collect::<Vec<_>>();
        format!("修复验证失败：{}", hints.join("；"))
    };

    let risks = if exec.plan.risks.is_empty() {
        "无".to_string()
    } else {
        exec.plan.risks.join("；")
    };

    format!(
        "结论=完成:{} | 未完成:{} | 风险:{}",
        completed.join("、"),
        pending,
        risks
    )
}

pub(super) fn summarize_verify_for_result(
    verify_records: &[VerifyExecutionRecord],
) -> Option<String> {
    if verify_records.is_empty() {
        return None;
    }

    let passed = verify_records.iter().filter(|record| record.ok).count();
    let failed = verify_records.len().saturating_sub(passed);
    let slowest_ms = verify_records
        .iter()
        .map(|record| record.duration_ms)
        .max()
        .unwrap_or(0);
    let output_bytes = verify_records
        .iter()
        .map(|record| record.stdout.len() + record.stderr.len())
        .sum::<usize>();
    let first_failure = verify_records
        .iter()
        .find(|record| !record.ok)
        .map(|record| {
            let detail = first_non_empty_line(&record.stderr)
                .or_else(|| first_non_empty_line(&record.stdout))
                .unwrap_or_else(|| "无".to_string());
            format!(
                "; first_failure={} (exit_code={}) detail={}",
                record.command, record.exit_code, detail
            )
        })
        .unwrap_or_default();
    Some(format!(
        "verify_passed={}/{}; verify_failed={}; verify_slowest_ms={}; verify_output_bytes={}{}",
        passed,
        verify_records.len(),
        failed,
        slowest_ms,
        output_bytes,
        first_failure
    ))
}

fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}
