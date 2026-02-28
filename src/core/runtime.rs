use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::agent_config::AgentConfig;
use crate::core::agents::execution_agent::{self, ExecutionStepReport};
use crate::core::agents::planning_agent;
use crate::core::agents::response_agent;
pub use crate::core::agents::response_agent::VerifyExecutionRecord;
use crate::core::mcp::collect_mcp_context;
use crate::core::model::{
    ModelClient, ModelRequest, ModelResponse, ModelStreamChunk, SingleProviderClient, TokenUsage,
};
use crate::core::planner::{PlanStepStatus, TaskPlan};
use crate::core::session::{Message, Session, now_text};
use crate::core::tool::{LocalToolExecutor, ToolExecutionRecord, ToolResult};

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

    #[allow(clippy::too_many_arguments)]
    pub fn execute_turn_with_streaming<F, P, L, T, S>(
        &self,
        session: &Session,
        user_input: &str,
        mut on_plan_ready: P,
        mut on_chunk: F,
        mut on_llm_output: L,
        mut on_tool_result: T,
        mut on_plan_execution_summary: S,
    ) -> Result<TurnExecution>
    where
        P: FnMut(&TaskPlan),
        F: FnMut(&ModelStreamChunk),
        L: FnMut(&LlmOutputRecord),
        T: FnMut(&ToolResult),
        S: FnMut(&str),
    {
        let (mut plan, planning_output) = planning_agent::build_plan_with_agent_with_trace(
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
            &mut on_tool_result,
            &mut on_plan_execution_summary,
            &mut on_plan_ready,
        )?;
        let verify_commands = recommend_verify_commands(user_input);
        let verify_records = run_verify_commands(&verify_commands);

        let reply_prompt = response_agent::build_grounded_response_prompt(
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
    fn execute_plan_steps_with_execution_agent<P, L, T, S>(
        &self,
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
                let tool_results_before = tool_results.len();
                let step_result = execution_agent::execute_single_plan_step_with_execution_agent(
                    &self.client,
                    &self.tool_executor,
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
                                stage: format!(
                                    "execution-agent::{current_plan_name}::{}",
                                    step.name
                                ),
                                content: output.content,
                                reasoning_content: output.reasoning_content,
                                tool_calls: output.tool_calls,
                            };
                            on_llm_output(&record);
                        }
                        mark_step_status(plan, plan_idx, step_idx, PlanStepStatus::Completed);
                        reports.push(step_result.report);
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

fn contains_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| text.contains(keyword))
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
