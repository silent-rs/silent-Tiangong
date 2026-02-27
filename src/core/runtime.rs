use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::core::agent_config::AgentConfig;
use crate::core::mcp::collect_mcp_context;
use crate::core::model::{
    FunctionToolSpec, ModelClient, ModelFunctionCall, ModelRequest, ModelResponse,
    ModelStreamChunk, SingleProviderClient, TokenUsage,
};
use crate::core::planner::{PlanStep, PlanStepStatus, TaskPlan, build_plan_with_agent_with_trace};
use crate::core::session::{Message, Session, now_text};
use crate::core::tool::{
    LocalToolExecutor, ToolCall, ToolExecutionRecord, ToolExecutor, ToolName, ToolResult,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Idle,
    Planning,
    Executing,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub status: RunStatus,
    pub summary: String,
    pub last_session_id: Option<String>,
    pub last_task_id: Option<String>,
    pub last_duration_ms: Option<u64>,
    pub last_result: Option<String>,
    pub last_plan: Option<String>,
    pub last_tool_result: Option<String>,
    pub last_error: Option<String>,
    pub last_usage: Option<TokenUsage>,
    pub updated_at: String,
}

impl Default for RunSnapshot {
    fn default() -> Self {
        Self {
            status: RunStatus::Idle,
            summary: "系统就绪".to_string(),
            last_session_id: None,
            last_task_id: None,
            last_duration_ms: None,
            last_result: None,
            last_plan: None,
            last_tool_result: None,
            last_error: None,
            last_usage: None,
            updated_at: now_text(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurnExecution {
    pub assistant_message: String,
    pub assistant_reasoning_content: String,
    pub plan: TaskPlan,
    pub tool_result_summary: Option<String>,
    pub tool_execution: Option<ToolExecutionRecord>,
    pub verify_records: Vec<VerifyExecutionRecord>,
    pub output_mode: String,
    pub output_chunk_count: usize,
    pub usage: TokenUsage,
}

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

#[derive(Debug, Clone)]
pub struct LlmOutputRecord {
    pub stage: String,
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeEngine {
    client: SingleProviderClient,
    tool_executor: LocalToolExecutor,
    context_limit: usize,
    agent_config: AgentConfig,
}

impl RuntimeEngine {
    pub fn new(
        client: SingleProviderClient,
        context_limit: usize,
        agent_config: AgentConfig,
    ) -> Self {
        Self {
            client,
            tool_executor: LocalToolExecutor,
            context_limit,
            agent_config,
        }
    }

    pub fn provider_label(&self) -> String {
        format!(
            "{} @ {} · {}ms",
            self.client.api_model(),
            self.client.api_base_url(),
            self.client.api_timeout_ms()
        )
    }

    pub fn execute_turn_with_streaming<F, P, L>(
        &self,
        session: &Session,
        user_input: &str,
        mut on_plan_ready: P,
        mut on_chunk: F,
        mut on_llm_output: L,
    ) -> Result<TurnExecution>
    where
        P: FnMut(&TaskPlan),
        F: FnMut(&ModelStreamChunk),
        L: FnMut(&LlmOutputRecord),
    {
        let (mut plan, planning_output) = build_plan_with_agent_with_trace(
            &self.client,
            session,
            user_input,
            &self.agent_config,
            self.context_limit,
        );
        if !planning_output.content.trim().is_empty()
            || !planning_output.reasoning_content.trim().is_empty()
        {
            let output = LlmOutputRecord {
                stage: "planning-agent".to_string(),
                content: planning_output.content,
                reasoning_content: planning_output.reasoning_content,
                tool_calls: Vec::new(),
            };
            on_llm_output(&output);
        }
        on_plan_ready(&plan);
        let context = session.recent_messages(self.context_limit);
        let mut tool_results = Vec::new();

        let mcp_context = collect_mcp_context(user_input, &self.agent_config.mcp);
        let mut execution_input = user_input.to_string();
        if !mcp_context.is_empty() {
            execution_input.push_str("\n\nMCP上下文：\n");
            for item in &mcp_context {
                execution_input.push_str("- ");
                execution_input.push_str(item);
                execution_input.push('\n');
            }
        }
        self.execute_plan_steps_with_execution_agent(
            &mut plan,
            session,
            &execution_input,
            &context,
            &mut tool_results,
            &mut on_llm_output,
            &mut on_plan_ready,
        )?;
        let verify_commands = recommend_verify_commands(user_input);
        let verify_records = run_verify_commands(&verify_commands);

        let reply_prompt = build_grounded_response_prompt(
            user_input,
            &plan,
            &tool_results,
            &verify_records,
            &mcp_context,
        );
        let req = ModelRequest {
            session_title: session.title.clone(),
            user_input: reply_prompt,
            context: context.clone(),
        };
        let ModelResponse {
            text,
            reasoning_content,
            usage,
            output_mode,
            output_chunk_count,
        } = if use_stream_mode() {
            match self
                .client
                .complete_stream_with_callback(&req, |delta| on_chunk(delta))
            {
                Ok(resp) => resp,
                Err(_) => {
                    let resp = self.client.complete(&req)?;
                    if !resp.reasoning_content.is_empty() {
                        on_chunk(&ModelStreamChunk {
                            content: String::new(),
                            reasoning_content: resp.reasoning_content.clone(),
                        });
                    }
                    if !resp.text.is_empty() {
                        on_chunk(&ModelStreamChunk {
                            content: resp.text.clone(),
                            reasoning_content: String::new(),
                        });
                    }
                    resp
                }
            }
        } else {
            let resp = self.client.complete(&req)?;
            if !resp.reasoning_content.is_empty() {
                on_chunk(&ModelStreamChunk {
                    content: String::new(),
                    reasoning_content: resp.reasoning_content.clone(),
                });
            }
            if !resp.text.is_empty() {
                on_chunk(&ModelStreamChunk {
                    content: resp.text.clone(),
                    reasoning_content: String::new(),
                });
            }
            resp
        };

        let tool_result_summary = summarize_tool_results(&tool_results);
        let tool_execution = tool_results
            .into_iter()
            .filter_map(|result| result.execution)
            .next_back();

        Ok(TurnExecution {
            assistant_message: text,
            assistant_reasoning_content: reasoning_content,
            plan,
            tool_result_summary,
            tool_execution,
            verify_records,
            output_mode,
            output_chunk_count,
            usage,
        })
    }

    pub fn fallback_error_message(err: &anyhow::Error) -> String {
        format!("执行失败：{err}")
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_plan_steps_with_execution_agent<P, L>(
        &self,
        plan: &mut TaskPlan,
        session: &Session,
        user_input: &str,
        context: &[Message],
        tool_results: &mut Vec<ToolResult>,
        on_llm_output: &mut L,
        on_plan_ready: &mut P,
    ) -> Result<()>
    where
        P: FnMut(&TaskPlan),
        L: FnMut(&LlmOutputRecord),
    {
        let mut previous_plan_summaries = Vec::new();

        for plan_idx in 0..plan.plans.len() {
            if plan.plans[plan_idx].status != PlanStepStatus::Pending {
                continue;
            }

            let current_plan_name = plan.plans[plan_idx].name.clone();
            let step_count = plan.plans[plan_idx].execution_steps.len();
            let mut reports = Vec::new();
            if let Some(item) = plan.plans.get_mut(plan_idx) {
                item.execution_summary = Some("执行中：准备执行当前 plan".to_string());
            }
            on_plan_ready(plan);

            for step_idx in 0..step_count {
                if plan.plans[plan_idx].execution_steps[step_idx].status != PlanStepStatus::Pending
                {
                    continue;
                }

                let step = plan.plans[plan_idx].execution_steps[step_idx].clone();
                match self.execute_single_plan_step_with_execution_agent(
                    session,
                    user_input,
                    context,
                    plan,
                    &current_plan_name,
                    &step,
                    &previous_plan_summaries,
                    tool_results,
                    on_llm_output,
                ) {
                    Ok(report) => {
                        mark_step_status(plan, plan_idx, step_idx, PlanStepStatus::Completed);
                        reports.push(report);
                    }
                    Err(err) => {
                        mark_step_status(plan, plan_idx, step_idx, PlanStepStatus::Failed);
                        reports.push(PlanStepExecutionReport {
                            step_name: step.name.clone(),
                            status: PlanStepStatus::Failed,
                            summary: err.to_string(),
                        });
                        let ignored = mark_remaining_steps_ignored(plan, plan_idx, step_idx + 1);
                        for ignored_step in ignored {
                            reports.push(PlanStepExecutionReport {
                                step_name: ignored_step.name,
                                status: PlanStepStatus::Ignored,
                                summary: "前置步骤失败，本步骤已忽略".to_string(),
                            });
                        }
                        break;
                    }
                }
                on_plan_ready(plan);
            }

            if let Some(item) = plan.plans.get_mut(plan_idx) {
                let summary = summarize_plan_execution(item.name.as_str(), &reports);
                item.execution_summary = Some(summary.clone());
                item.refresh_status();
                previous_plan_summaries.push(summary);
            }
            plan.refresh_plan_statuses();
            on_plan_ready(plan);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_single_plan_step_with_execution_agent(
        &self,
        session: &Session,
        user_input: &str,
        context: &[Message],
        plan: &TaskPlan,
        plan_name: &str,
        step: &PlanStep,
        previous_plan_summaries: &[String],
        tool_results: &mut Vec<ToolResult>,
        on_llm_output: &mut impl FnMut(&LlmOutputRecord),
    ) -> Result<PlanStepExecutionReport> {
        let request = ModelRequest {
            session_title: format!("{} · execution-agent", session.title),
            user_input: build_step_execution_prompt(
                user_input,
                plan,
                plan_name,
                step,
                previous_plan_summaries,
            ),
            context: context.to_vec(),
        };
        let response = self
            .client
            .complete_with_functions(&request, &basic_file_function_tools())?;
        let llm_tool_calls = response
            .tool_calls
            .iter()
            .map(|call| call.name.clone())
            .collect::<Vec<_>>();
        if !response.text.trim().is_empty()
            || !response.reasoning_content.trim().is_empty()
            || !llm_tool_calls.is_empty()
        {
            let output = LlmOutputRecord {
                stage: format!("execution-agent::{plan_name}::{}", step.name),
                content: response.text.clone(),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: llm_tool_calls,
            };
            on_llm_output(&output);
        }
        let _ = (
            response.text.as_str(),
            response.reasoning_content.as_str(),
            response.usage.total_tokens,
        );

        if response.tool_calls.is_empty() {
            return Err(anyhow!(
                "执行智能体未提交任何函数调用：step={} {}",
                step.name,
                step.description
            ));
        }

        let mut step_completed = false;
        let mut executed_tools = Vec::new();
        for tool_call in &response.tool_calls {
            let _function_call_id = tool_call.id.as_str();
            if tool_call.name == "mark_step_completed" {
                step_completed = true;
                continue;
            }
            let call = build_tool_call_from_function(tool_call)?;
            let result = self.tool_executor.execute(&call)?;
            if let Some(execution) = result.execution.as_ref() {
                executed_tools.push(execution.tool_name.clone());
            }
            tool_results.push(result.clone());
            if !result.ok {
                return Err(anyhow!("{}", build_tool_failure_error(&result)));
            }
        }
        if !step_completed {
            let tool_hint = if executed_tools.is_empty() {
                "未调用任何工具".to_string()
            } else {
                format!("已调用工具：{}", executed_tools.join(","))
            };
            return Err(anyhow!(
                "执行智能体未显式提交步骤完成信号（mark_step_completed）：step={} {}；{}",
                step.name,
                step.description,
                tool_hint
            ));
        }
        let tool_summary = if executed_tools.is_empty() {
            "步骤完成：未调用外部工具".to_string()
        } else {
            format!("步骤完成：工具成功：{}", executed_tools.join(","))
        };
        Ok(PlanStepExecutionReport {
            step_name: step.name.clone(),
            status: PlanStepStatus::Completed,
            summary: tool_summary,
        })
    }
}

fn use_stream_mode() -> bool {
    match std::env::var("API_STREAM") {
        Ok(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "0" | "false" | "off" | "no")
        }
        Err(_) => true,
    }
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

fn summarize_tool_results(results: &[ToolResult]) -> Option<String> {
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

fn build_tool_failure_error(result: &ToolResult) -> String {
    let tool_name = result
        .execution
        .as_ref()
        .map(|record| record.tool_name.as_str())
        .unwrap_or("unknown");
    let stderr_line = result
        .stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .to_string();
    if stderr_line.is_empty() {
        format!("工具调用失败：tool={tool_name}，summary={}", result.summary)
    } else {
        format!(
            "工具调用失败：tool={tool_name}，summary={}，stderr={stderr_line}",
            result.summary
        )
    }
}

#[derive(Debug, Clone)]
struct PlanStepExecutionReport {
    step_name: String,
    status: PlanStepStatus,
    summary: String,
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

fn summarize_plan_execution(plan_name: &str, reports: &[PlanStepExecutionReport]) -> String {
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

fn contains_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| text.contains(keyword))
}

fn build_step_execution_prompt(
    user_input: &str,
    plan: &TaskPlan,
    plan_name: &str,
    step: &PlanStep,
    previous_plan_summaries: &[String],
) -> String {
    let plan_snapshot = format_plan_snapshot(plan);
    let previous_plan_result_text = if previous_plan_summaries.is_empty() {
        "无".to_string()
    } else {
        previous_plan_summaries
            .iter()
            .enumerate()
            .map(|(idx, summary)| format!("{}. {}", idx + 1, summary))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        r#"你是执行智能体，负责执行当前 plan 步骤并确保结果可落地。

约束：
1. 只围绕“当前步骤”执行，不要改写 plan，不要新增步骤。
2. 需要文件/命令操作时，必须调用可用工具完成。
3. 完成步骤时，必须调用 `mark_step_completed` 函数作为完成信号。
4. 若调用了工具，`mark_step_completed` 必须在所有工具调用成功后再调用。
5. 不要输出冗长解释，聚焦执行结果。

用户输入：
{user_input}

当前 plan：
{plan_name}

已完成 plan 的执行汇总（仅供参考）：
{previous_plan_result_text}

当前计划快照：
{plan_snapshot}

当前步骤：
- name: {step_name}
- description: {step_desc}"#,
        plan_name = plan_name,
        previous_plan_result_text = previous_plan_result_text,
        step_name = step.name,
        step_desc = step.description
    )
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

fn build_grounded_response_prompt(
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
        r#"你是执行结果汇总助手。请严格基于“执行证据”回答用户，不得编造目录、文件、命令输出或技术栈。

要求：
1. 只能使用下方证据中的事实；若证据不足，明确说“未查询到”或“无法确认”。
2. 若存在失败步骤，必须先说明失败点与影响，再给出当前可确认信息。
3. 不要把推测写成确定事实。
4. 直接回答用户问题，语言简洁。

用户原始问题：
{user_input}

MCP上下文：
{mcp_summary}

计划执行快照：
{plan_snapshot}

工具执行证据：
{tool_evidence}

验证命令结果：
{verify_summary}"#
    )
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

fn basic_file_function_tools() -> Vec<FunctionToolSpec> {
    vec![
        FunctionToolSpec {
            name: "list_dir".to_string(),
            description: "列出目录中的文件和子目录".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录路径，默认当前目录" }
                },
                "required": []
            }),
        },
        FunctionToolSpec {
            name: "tree_dir".to_string(),
            description: "按目录树格式列出目录，支持通过 max_depth 限制遍历深度".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录路径，默认当前目录" },
                    "max_depth": {
                        "type": "integer",
                        "description": "遍历最大深度，建议 1-4，默认 2，最大 8",
                        "minimum": 0,
                        "maximum": 8
                    }
                },
                "required": []
            }),
        },
        FunctionToolSpec {
            name: "read_file".to_string(),
            description: "读取文件内容".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" }
                },
                "required": ["path"]
            }),
        },
        FunctionToolSpec {
            name: "write_file".to_string(),
            description: "创建或覆盖文件内容".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "content": { "type": "string", "description": "要写入的完整内容" }
                },
                "required": ["path", "content"]
            }),
        },
        FunctionToolSpec {
            name: "replace_in_file".to_string(),
            description: "在文件中将旧文本替换为新文本".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "old": { "type": "string", "description": "待替换的旧文本" },
                    "new": { "type": "string", "description": "替换后的新文本" }
                },
                "required": ["path", "old", "new"]
            }),
        },
        FunctionToolSpec {
            name: "run_command".to_string(),
            description: "执行受控命令，参数为命令名与参数数组。bash 请使用 cmd=bash,args=[\"-lc\",\"<script>\"]".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "命令名，例如 ls/cat/echo/pwd/bash" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "命令参数列表"
                    }
                },
                "required": ["cmd"]
            }),
        },
        FunctionToolSpec {
            name: "apply_patch".to_string(),
            description: "对文件应用补丁文本".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "补丁内容文本" }
                },
                "required": ["patch"]
            }),
        },
        FunctionToolSpec {
            name: "mark_step_completed".to_string(),
            description: "标记当前执行步骤已完成。仅在本步骤真正完成后调用。".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "result": { "type": "string", "description": "本步骤完成结果摘要" }
                },
                "required": []
            }),
        },
    ]
}

fn build_tool_call_from_function(call: &ModelFunctionCall) -> Result<ToolCall> {
    let mut args = Vec::new();
    let tool_call = match call.name.as_str() {
        "list_dir" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            args.push(path);
            Ok(ToolCall {
                name: ToolName::ListDir,
                args,
            })
        }
        "tree_dir" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            let max_depth = call
                .arguments
                .get("max_depth")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value.to_string())
                .or_else(|| {
                    call.arguments
                        .get("max_depth")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                })
                .unwrap_or_else(|| "2".to_string());
            args.push(path);
            args.push(max_depth);
            Ok(ToolCall {
                name: ToolName::TreeDir,
                args,
            })
        }
        "read_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(path);
            Ok(ToolCall {
                name: ToolName::ReadFile,
                args,
            })
        }
        "write_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let content = call
                .arguments
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(path);
            args.push(content);
            Ok(ToolCall {
                name: ToolName::WriteFile,
                args,
            })
        }
        "replace_in_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let old = call
                .arguments
                .get("old")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let new = call
                .arguments
                .get("new")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(path);
            args.push(old);
            args.push(new);
            Ok(ToolCall {
                name: ToolName::ReplaceInFile,
                args,
            })
        }
        "run_command" => {
            let cmd = call
                .arguments
                .get("cmd")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(cmd);
            if let Some(arr) = call
                .arguments
                .get("args")
                .and_then(serde_json::Value::as_array)
            {
                args.extend(
                    arr.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(ToString::to_string),
                );
            }
            Ok(ToolCall {
                name: ToolName::RunCommand,
                args,
            })
        }
        "run_bash" => {
            let script = call
                .arguments
                .get("script")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push("bash".to_string());
            args.push("-lc".to_string());
            args.push(script);
            Ok(ToolCall {
                name: ToolName::RunCommand,
                args,
            })
        }
        "apply_patch" => {
            let patch = call
                .arguments
                .get("patch")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(patch);
            Ok(ToolCall {
                name: ToolName::ApplyPatch,
                args,
            })
        }
        _ => Err(anyhow!("未知函数调用：{}", call.name)),
    }?;
    Ok(tool_call)
}

fn recommend_verify_commands(user_input: &str) -> Vec<String> {
    let text = user_input.to_ascii_lowercase();
    let likely_code_task = contains_any(
        &text,
        &[
            "rust", ".rs", "代码", "编译", "构建", "check", "clippy", "cargo", "修复", "重构",
        ],
    );
    if !likely_code_task {
        return Vec::new();
    }

    let mut commands = vec!["cargo check --workspace".to_string()];
    if contains_any(&text, &["clippy", "严格", "-d warnings"]) {
        commands.push(
            "cargo clippy --workspace --all-targets --tests --benches -- -D warnings".to_string(),
        );
    }
    commands
}

fn run_verify_commands(commands: &[String]) -> Vec<VerifyExecutionRecord> {
    let mut records = Vec::new();
    let timeout_ms = verify_command_timeout_ms();

    for command in commands
        .iter()
        .map(|cmd| cmd.trim())
        .filter(|cmd| !cmd.is_empty() && *cmd != "无")
    {
        let parts = command
            .split_whitespace()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let Some(program) = parts.first().cloned() else {
            continue;
        };
        let args = parts.iter().skip(1).cloned().collect::<Vec<_>>();

        let started = Instant::now();
        if !is_allowed_verify_command(&program, &args) {
            records.push(VerifyExecutionRecord {
                command: command.to_string(),
                ok: false,
                exit_code: 1,
                duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                summary: format!("验证命令不在允许列表：{command}"),
                stdout: String::new(),
                stderr: String::new(),
            });
            continue;
        }

        let workspace = match std::env::current_dir() {
            Ok(path) => path,
            Err(err) => {
                records.push(VerifyExecutionRecord {
                    command: command.to_string(),
                    ok: false,
                    exit_code: 1,
                    duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                    summary: format!("读取工作目录失败：{err}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                });
                continue;
            }
        };

        let outcome = execute_command_with_timeout(
            Command::new(&program)
                .args(&args)
                .current_dir(workspace)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        )
        .with_context(|| format!("执行验证命令失败：{command}"));

        match outcome {
            Ok((output, timed_out)) => {
                let mut exit_code = output.status.code().unwrap_or(-1);
                if timed_out {
                    exit_code = -1;
                }
                let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
                let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
                let ok = !timed_out && output.status.success();
                let summary = if timed_out {
                    format!("验证超时：{command} (timeout_ms={timeout_ms})")
                } else if ok {
                    format!("验证通过：{command}")
                } else {
                    let detail = extract_actionable_error(&stderr, &stdout)
                        .unwrap_or_else(|| "无错误详情".to_string());
                    format!("验证失败：{command} (exit_code={exit_code})，建议先处理：{detail}")
                };

                records.push(VerifyExecutionRecord {
                    command: command.to_string(),
                    ok,
                    exit_code,
                    duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                    summary,
                    stdout,
                    stderr,
                });
            }
            Err(err) => {
                records.push(VerifyExecutionRecord {
                    command: command.to_string(),
                    ok: false,
                    exit_code: 1,
                    duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                    summary: format!("验证命令执行异常：{command}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                });
            }
        }
    }

    records
}

fn verify_command_timeout_ms() -> u64 {
    const DEFAULT_TIMEOUT_MS: u64 = 120_000;
    std::env::var("VERIFY_COMMAND_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
}

fn is_allowed_verify_command(program: &str, args: &[String]) -> bool {
    if program == "cargo" {
        return args
            .first()
            .map(|sub| matches!(sub.as_str(), "check" | "clippy" | "build" | "test"))
            .unwrap_or(false);
    }
    if matches!(program, "cat" | "head" | "tail" | "wc" | "ls") {
        return validate_verify_paths(args);
    }
    false
}

fn validate_verify_paths(args: &[String]) -> bool {
    let workspace_root = match env::current_dir() {
        Ok(path) => normalize_path(path),
        Err(_) => return false,
    };
    let temp_root = normalize_path(env::temp_dir());

    for arg in args {
        let trimmed = arg.trim();
        if trimmed.is_empty() || trimmed.starts_with('-') {
            continue;
        }
        if trimmed.parse::<i64>().is_ok() {
            continue;
        }

        let path = normalize_path(if PathBuf::from(trimmed).is_absolute() {
            PathBuf::from(trimmed)
        } else {
            workspace_root.join(trimmed)
        });
        if !(path.starts_with(&workspace_root) || path.starts_with(&temp_root)) {
            return false;
        }
    }
    true
}

fn normalize_path(path: PathBuf) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn execute_command_with_timeout(command: &mut Command, timeout_ms: u64) -> Result<(Output, bool)> {
    let mut child = command.spawn().context("spawn 子进程失败")?;
    let timeout = Duration::from_millis(timeout_ms);
    let started = Instant::now();

    loop {
        if let Some(_status) = child.try_wait().context("轮询子进程状态失败")? {
            let output = child.wait_with_output().context("读取命令输出失败")?;
            return Ok((output, false));
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            let output = child.wait_with_output().context("读取超时命令输出失败")?;
            return Ok((output, true));
        }

        thread::sleep(Duration::from_millis(20));
    }
}

fn extract_actionable_error(stderr: &str, stdout: &str) -> Option<String> {
    for raw in stderr.lines().chain(stdout.lines()) {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains("error")
            || line.contains("Error")
            || line.contains("failed")
            || line.contains("warning:")
        {
            return Some(line.chars().take(220).collect());
        }
    }
    None
}

fn truncate_output(raw: &str) -> String {
    const MAX_CHARS: usize = 4000;
    let mut output = raw.chars().take(MAX_CHARS).collect::<String>();
    if raw.chars().count() > MAX_CHARS {
        output.push_str("\n...(truncated)");
    }
    output
}

fn elapsed_ms_u64(raw: u128) -> u64 {
    raw.min(u64::MAX as u128) as u64
}
