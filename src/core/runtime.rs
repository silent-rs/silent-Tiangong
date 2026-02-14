use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::agent_config::AgentConfig;
use crate::core::mcp::collect_mcp_context;
use crate::core::model::{
    ModelClient, ModelRequest, ModelResponse, ModelStreamChunk, SingleProviderClient, TokenUsage,
};
use crate::core::planner::{TaskPlan, build_minimal_plan};
use crate::core::session::{Session, now_text};
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

    pub fn execute_turn_with_streaming<F>(
        &self,
        session: &Session,
        user_input: &str,
        mut on_chunk: F,
    ) -> Result<TurnExecution>
    where
        F: FnMut(&ModelStreamChunk),
    {
        let plan = build_minimal_plan(user_input, &self.agent_config);
        let context = session.recent_messages(self.context_limit);
        let tool_result = self.maybe_execute_tool(user_input)?;
        let tool_result_summary = tool_result.as_ref().map(format_tool_result_for_display);
        let mcp_context = collect_mcp_context(user_input, &self.agent_config.mcp);
        let mut prompt = user_input.to_string();
        if let Some(summary) = tool_result.as_ref().map(|result| result.summary.clone()) {
            prompt.push_str(&format!("\n\n工具预执行摘要：{summary}"));
        }
        if !mcp_context.is_empty() {
            prompt.push_str("\n\nMCP上下文：\n");
            for item in &mcp_context {
                prompt.push_str("- ");
                prompt.push_str(item);
                prompt.push('\n');
            }
        }

        let req = ModelRequest {
            session_title: session.title.clone(),
            user_input: prompt,
            context,
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
        let verify_records = run_verify_commands(&plan.verify_commands);

        Ok(TurnExecution {
            assistant_message: text,
            assistant_reasoning_content: reasoning_content,
            plan,
            tool_result_summary,
            tool_execution: tool_result.and_then(|result| result.execution),
            verify_records,
            output_mode,
            output_chunk_count,
            usage,
        })
    }

    fn maybe_execute_tool(&self, user_input: &str) -> Result<Option<ToolResult>> {
        if !(user_input.contains("目录")
            || user_input.contains("文件")
            || user_input.contains("命令")
            || user_input.contains("搜索")
            || user_input.contains("查找")
            || user_input.contains("grep")
            || user_input.contains("rg"))
        {
            return Ok(None);
        }

        let call = if user_input.contains("搜索")
            || user_input.contains("查找")
            || user_input.contains("grep")
            || user_input.contains("rg")
        {
            ToolCall {
                name: ToolName::SearchCode,
                args: vec![infer_search_pattern(user_input), ".".to_string()],
            }
        } else if user_input.contains("目录") {
            ToolCall {
                name: ToolName::ListDir,
                args: vec![".".to_string()],
            }
        } else if user_input.contains("命令") {
            ToolCall {
                name: ToolName::RunCommand,
                args: vec!["echo".to_string(), "phase1".to_string()],
            }
        } else {
            ToolCall {
                name: ToolName::ReadFile,
                args: vec!["README.md".to_string()],
            }
        };

        let result = self.tool_executor.execute(&call)?;
        Ok(Some(result))
    }

    pub fn fallback_error_message(err: &anyhow::Error) -> String {
        format!("执行失败：{err}")
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

fn infer_search_pattern(user_input: &str) -> String {
    if let Some(pattern) = extract_between(user_input, '"', '"') {
        return pattern;
    }
    if let Some(pattern) = extract_between(user_input, '“', '”') {
        return pattern;
    }
    if let Some(pattern) = extract_between(user_input, '`', '`') {
        return pattern;
    }
    if user_input.contains("TODO") {
        return "TODO".to_string();
    }
    if user_input.contains("FIXME") {
        return "FIXME".to_string();
    }
    "main".to_string()
}

fn extract_between(input: &str, start: char, end: char) -> Option<String> {
    let start_pos = input.find(start)?;
    let tail = input.get(start_pos + start.len_utf8()..)?;
    let end_rel = tail.find(end)?;
    let value = tail.get(..end_rel)?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
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
    false
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
