use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::agent_config::AgentConfig;
use crate::core::mcp::collect_mcp_context;
use crate::core::model::{
    FunctionToolSpec, ModelClient, ModelFunctionCall, ModelRequest, ModelResponse,
    ModelStreamChunk, SingleProviderClient, TokenUsage,
};
use crate::core::planner::{PlanStep, PlanStepStatus, TaskPlan, build_plan_with_agent};
use crate::core::session::{Message, MessageRole, Session, now_text};
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

    pub fn execute_turn_with_streaming<F, P>(
        &self,
        session: &Session,
        user_input: &str,
        mut on_plan_ready: P,
        mut on_chunk: F,
    ) -> Result<TurnExecution>
    where
        P: FnMut(&TaskPlan),
        F: FnMut(&ModelStreamChunk),
    {
        let mut plan = build_plan_with_agent(
            &self.client,
            session,
            user_input,
            &self.agent_config,
            self.context_limit,
        );
        on_plan_ready(&plan);
        let context = session.recent_messages(self.context_limit);
        let mut tool_results = Vec::new();
        let used_function_calls = self.execute_file_tools_with_function_call(
            session,
            user_input,
            &context,
            &mut plan,
            &mut tool_results,
            &mut on_chunk,
            &mut on_plan_ready,
        )?;
        if !used_function_calls {
            self.execute_pre_model_plan_steps(
                &mut plan,
                user_input,
                &mut tool_results,
                &mut on_plan_ready,
            )?;
            if tool_results.is_empty()
                && let Some(result) = self.maybe_execute_tool(user_input)?
            {
                if revise_plan_for_tool_result(&mut plan, Some(&result)) {
                    on_plan_ready(&plan);
                }
                if result.ok
                    && let Some(location) = next_pending_step_location(&plan)
                {
                    mark_step_completed(&mut plan, location);
                    on_plan_ready(&plan);
                }
                tool_results.push(result);
            }
        }

        let mcp_context = collect_mcp_context(user_input, &self.agent_config.mcp);
        let mut prompt = user_input.to_string();
        if let Some(summary) = summarize_tool_results(&tool_results) {
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
        self.complete_model_step_if_needed(&mut plan, &mut on_plan_ready);
        self.execute_post_model_plan_steps(
            &mut plan,
            session,
            user_input,
            &text,
            &mut tool_results,
            &mut on_plan_ready,
        )?;
        let verify_commands = recommend_verify_commands(user_input);
        let verify_records = run_verify_commands(&verify_commands);

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

    fn execute_pre_model_plan_steps<P>(
        &self,
        plan: &mut TaskPlan,
        user_input: &str,
        tool_results: &mut Vec<ToolResult>,
        on_plan_ready: &mut P,
    ) -> Result<()>
    where
        P: FnMut(&TaskPlan),
    {
        loop {
            let Some(location) = next_pending_step_location(plan) else {
                break;
            };
            let step = plan.plans[location.plan_idx].execution_steps[location.step_idx].clone();
            let Some(call) = infer_pre_model_tool_call(user_input, &step) else {
                break;
            };

            let result = self.tool_executor.execute(&call)?;
            if revise_plan_for_tool_result(plan, Some(&result)) {
                on_plan_ready(plan);
            }
            if result.ok {
                mark_step_completed(plan, location);
                on_plan_ready(plan);
            } else {
                on_plan_ready(plan);
            }
            tool_results.push(result.clone());
            if !result.ok {
                break;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_file_tools_with_function_call<F, P>(
        &self,
        session: &Session,
        user_input: &str,
        context: &[Message],
        plan: &mut TaskPlan,
        tool_results: &mut Vec<ToolResult>,
        on_chunk: &mut F,
        on_plan_ready: &mut P,
    ) -> Result<bool>
    where
        F: FnMut(&ModelStreamChunk),
        P: FnMut(&TaskPlan),
    {
        if !should_use_file_function_calls(user_input) {
            return Ok(false);
        }

        let prompt = format!(
            "{}\n\n当涉及文件读取、写入、替换、补丁应用时，必须优先使用函数工具，不要只给出文字说明。",
            user_input
        );
        let request = ModelRequest {
            session_title: format!("{} · function-call", session.title),
            user_input: prompt,
            context: context.to_vec(),
        };
        let response = self
            .client
            .complete_with_functions(&request, &basic_file_function_tools())?;
        let _function_call_usage_total = response.usage.total_tokens;

        if !response.reasoning_content.trim().is_empty() {
            on_chunk(&ModelStreamChunk {
                content: String::new(),
                reasoning_content: response.reasoning_content.clone(),
            });
        }
        if !response.text.trim().is_empty() {
            on_chunk(&ModelStreamChunk {
                content: response.text.clone(),
                reasoning_content: String::new(),
            });
        }

        if response.tool_calls.is_empty() {
            return Ok(false);
        }

        for tool_call in &response.tool_calls {
            let _function_call_id = tool_call.id.as_str();
            let call = build_tool_call_from_function(tool_call);
            let result = self.tool_executor.execute(&call)?;
            if revise_plan_for_tool_result(plan, Some(&result)) {
                on_plan_ready(plan);
            }
            if result.ok
                && let Some(location) = next_pending_step_location(plan)
            {
                mark_step_completed(plan, location);
                on_plan_ready(plan);
            }
            tool_results.push(result);
        }
        Ok(true)
    }

    fn complete_model_step_if_needed<P>(&self, plan: &mut TaskPlan, on_plan_ready: &mut P)
    where
        P: FnMut(&TaskPlan),
    {
        let Some(location) = next_pending_step_location(plan) else {
            return;
        };
        if is_model_generation_step(
            &plan.plans[location.plan_idx].execution_steps[location.step_idx],
        ) {
            mark_step_completed(plan, location);
            on_plan_ready(plan);
        }
    }

    fn execute_post_model_plan_steps<P>(
        &self,
        plan: &mut TaskPlan,
        session: &Session,
        user_input: &str,
        assistant_text: &str,
        tool_results: &mut Vec<ToolResult>,
        on_plan_ready: &mut P,
    ) -> Result<()>
    where
        P: FnMut(&TaskPlan),
    {
        loop {
            let Some(location) = next_pending_step_location(plan) else {
                break;
            };
            let step = plan.plans[location.plan_idx].execution_steps[location.step_idx].clone();
            match infer_post_model_step_action(session, user_input, assistant_text, &step) {
                PostModelStepAction::MarkCompleted => {
                    mark_step_completed(plan, location);
                    on_plan_ready(plan);
                }
                PostModelStepAction::Tool(call) => {
                    let result = self.tool_executor.execute(&call)?;
                    if revise_plan_for_tool_result(plan, Some(&result)) {
                        on_plan_ready(plan);
                    }
                    if result.ok {
                        mark_step_completed(plan, location);
                    }
                    on_plan_ready(plan);
                    tool_results.push(result.clone());
                    if !result.ok {
                        break;
                    }
                }
                PostModelStepAction::Stop => break,
            }
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
enum PostModelStepAction {
    MarkCompleted,
    Tool(ToolCall),
    Stop,
}

#[derive(Debug, Clone, Copy)]
struct PlanStepLocation {
    plan_idx: usize,
    step_idx: usize,
}

fn next_pending_step_location(plan: &TaskPlan) -> Option<PlanStepLocation> {
    for (plan_idx, item) in plan.plans.iter().enumerate() {
        for (step_idx, step) in item.execution_steps.iter().enumerate() {
            if step.status == PlanStepStatus::Pending {
                return Some(PlanStepLocation { plan_idx, step_idx });
            }
        }
    }
    None
}

fn mark_step_completed(plan: &mut TaskPlan, location: PlanStepLocation) {
    if let Some(item) = plan.plans.get_mut(location.plan_idx) {
        if let Some(step) = item.execution_steps.get_mut(location.step_idx) {
            step.status = PlanStepStatus::Completed;
        }
        item.refresh_status();
    }
    plan.refresh_plan_statuses();
}

fn infer_pre_model_tool_call(user_input: &str, step: &PlanStep) -> Option<ToolCall> {
    let raw_text = format!("{} {}", step.name, step.description);
    let text = raw_text.to_ascii_lowercase();
    if contains_any(&text, &["读取", "read", "查看文件"]) {
        let path = infer_read_target_path(user_input, &raw_text)
            .unwrap_or_else(|| "README.md".to_string());
        return Some(ToolCall {
            name: ToolName::ReadFile,
            args: vec![path],
        });
    }
    if contains_any(&text, &["目录", "list_dir", "浏览文件"]) {
        return Some(ToolCall {
            name: ToolName::ListDir,
            args: vec![".".to_string()],
        });
    }
    if contains_any(&text, &["检索", "搜索", "查找", "search", "grep", "rg"]) {
        return Some(ToolCall {
            name: ToolName::SearchCode,
            args: vec![infer_search_pattern(user_input), ".".to_string()],
        });
    }
    None
}

fn infer_post_model_step_action(
    session: &Session,
    user_input: &str,
    assistant_text: &str,
    step: &PlanStep,
) -> PostModelStepAction {
    let raw_text = format!("{} {}", step.name, step.description);
    let text = raw_text.to_ascii_lowercase();
    if contains_any(&text, &["写入", "创建文件", "保存到", "落盘", "write_file"]) {
        let path = infer_write_target_path(user_input, &raw_text)
            .unwrap_or_else(|| "output.txt".to_string());
        let content = resolve_write_content(session, user_input, assistant_text);
        return PostModelStepAction::Tool(ToolCall {
            name: ToolName::WriteFile,
            args: vec![path, content],
        });
    }

    if contains_any(&text, &["完成", "确认", "收尾"]) {
        return PostModelStepAction::MarkCompleted;
    }

    if is_model_generation_step(step) {
        return PostModelStepAction::MarkCompleted;
    }

    PostModelStepAction::Stop
}

fn is_model_generation_step(step: &PlanStep) -> bool {
    let text = step_text(step);
    contains_any(
        &text,
        &[
            "generate",
            "response",
            "回答",
            "生成回答",
            "整合",
            "总结",
            "撰写",
            "输出",
            "草稿",
            "润色",
        ],
    )
}

fn step_text(step: &PlanStep) -> String {
    format!("{} {}", step.name, step.description).to_ascii_lowercase()
}

fn contains_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| text.contains(keyword))
}

fn should_use_file_function_calls(user_input: &str) -> bool {
    let text = user_input.to_ascii_lowercase();
    contains_any(
        &text,
        &[
            "文件",
            "目录",
            "读取",
            "读一下",
            "写入",
            "保存",
            "替换",
            "修改",
            "补丁",
            "apply patch",
            "read_file",
            "write_file",
            "replace_in_file",
            "apply_patch",
            "命令",
            "终端",
            "bash",
            "shell",
            "run_command",
        ],
    )
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
    ]
}

fn build_tool_call_from_function(call: &ModelFunctionCall) -> ToolCall {
    let mut args = Vec::new();
    match call.name.as_str() {
        "list_dir" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            args.push(path);
            ToolCall {
                name: ToolName::ListDir,
                args,
            }
        }
        "read_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(path);
            ToolCall {
                name: ToolName::ReadFile,
                args,
            }
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
            ToolCall {
                name: ToolName::WriteFile,
                args,
            }
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
            ToolCall {
                name: ToolName::ReplaceInFile,
                args,
            }
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
            ToolCall {
                name: ToolName::RunCommand,
                args,
            }
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
            ToolCall {
                name: ToolName::RunCommand,
                args,
            }
        }
        "apply_patch" => {
            let patch = call
                .arguments
                .get("patch")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(patch);
            ToolCall {
                name: ToolName::ApplyPatch,
                args,
            }
        }
        _ => ToolCall {
            name: ToolName::ReadFile,
            args: vec![format!("__unknown_function_call__:{}", call.name)],
        },
    }
}

fn infer_read_target_path(user_input: &str, step_text: &str) -> Option<String> {
    extract_file_path_candidate(step_text).or_else(|| extract_file_path_candidate(user_input))
}

fn infer_write_target_path(user_input: &str, step_text: &str) -> Option<String> {
    extract_file_path_candidate(step_text).or_else(|| extract_file_path_candidate(user_input))
}

fn resolve_write_content(session: &Session, user_input: &str, assistant_text: &str) -> String {
    let current = assistant_text.trim();
    let write_request = is_write_like_request(user_input);
    if write_request {
        if is_story_write_request(user_input) {
            let merged_stories = collect_story_outputs_from_session(session);
            if !merged_stories.is_empty() {
                return merged_stories.join("\n\n");
            }
        }
        if is_story_like_candidate(current) {
            return current.to_string();
        }
        if let Some(previous_body) = latest_story_like_content_from_session(session)
            && is_story_like_candidate(previous_body.trim())
        {
            return previous_body;
        }
    }
    if !current.is_empty() {
        return current.to_string();
    }
    latest_story_like_content_from_session(session).unwrap_or_default()
}

fn latest_story_like_content_from_session(session: &Session) -> Option<String> {
    session
        .messages
        .iter()
        .rev()
        .filter(|msg| matches!(msg.role, MessageRole::Assistant | MessageRole::User))
        .map(|msg| msg.content.trim())
        .find(|content| is_story_like_candidate(content))
        .map(ToString::to_string)
}

fn is_write_like_request(user_input: &str) -> bool {
    let text = user_input.to_ascii_lowercase();
    contains_any(
        &text,
        &[
            "保存", "写入", "落盘", "导出", "存到", "文件", "write", "save", "export",
        ],
    )
}

fn is_story_write_request(user_input: &str) -> bool {
    let text = user_input.to_ascii_lowercase();
    contains_any(&text, &["故事", "续写", "小说", "写到txt", "保存故事"])
}

fn collect_story_outputs_from_session(session: &Session) -> Vec<String> {
    let mut outputs = Vec::new();
    for (idx, message) in session.messages.iter().enumerate() {
        if !matches!(message.role, MessageRole::Assistant) {
            continue;
        }
        let content = message.content.trim();
        if content.is_empty()
            || looks_like_save_confirmation(content)
            || !is_story_like_candidate(content)
        {
            continue;
        }

        let triggered_by_story_prompt = (0..idx).rev().find_map(|cursor| {
            let prev = &session.messages[cursor];
            if matches!(prev.role, MessageRole::User) {
                Some(is_story_generation_prompt(prev.content.as_str()))
            } else {
                None
            }
        });

        if triggered_by_story_prompt.unwrap_or(false)
            && !outputs.iter().any(|existing: &String| existing == content)
        {
            outputs.push(content.to_string());
        }
    }
    outputs
}

fn is_story_generation_prompt(user_input: &str) -> bool {
    let text = user_input.to_ascii_lowercase();
    contains_any(
        &text,
        &["故事", "续写", "编一个", "讲个", "讲一个", "300字", "200字"],
    )
}

fn looks_like_save_confirmation(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    let confirmation_hit = contains_any(
        &normalized,
        &[
            "我已经",
            "已保存",
            "保存到",
            "文件已保存",
            "文件保存在",
            "写入完成",
            "已经将",
            "saved to",
            "has been saved",
            "file saved",
        ],
    );
    let likely_short_notice = text.chars().count() < 260;
    confirmation_hit && likely_short_notice
}

fn is_story_like_candidate(text: &str) -> bool {
    let body = text.trim();
    if body.is_empty() {
        return false;
    }
    if looks_like_save_confirmation(body) {
        return false;
    }
    let normalized = body.to_ascii_lowercase();
    if contains_any(
        &normalized,
        &[
            "文件名",
            "保存路径",
            "验证命令",
            "pending",
            "completed",
            "repair_after_verify_failure",
            "步骤",
            "计划",
        ],
    ) && body.chars().count() < 500
    {
        return false;
    }
    body.chars().count() >= 120
}

fn extract_file_path_candidate(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let cleaned = trim_token_punctuation(token);
        if cleaned.is_empty() {
            continue;
        }
        if is_file_like_token(&cleaned) {
            return Some(cleaned);
        }
    }
    None
}

fn trim_token_punctuation(token: &str) -> String {
    let mut trimmed = token.trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\''
                | '`'
                | '“'
                | '”'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | ','
                | ';'
                | ':'
                | '，'
                | '。'
                | '；'
                | '：'
        )
    });
    while let Some(ch) = trimmed.chars().last() {
        if matches!(ch, ',' | ';' | ':' | '，' | '。' | '；' | '：') {
            trimmed = &trimmed[..trimmed.len().saturating_sub(ch.len_utf8())];
        } else {
            break;
        }
    }
    trimmed.to_string()
}

fn is_file_like_token(token: &str) -> bool {
    let lowercase = token.to_ascii_lowercase();
    let has_separator = token.contains('/') || token.contains('\\');
    let extensions = [
        ".txt", ".md", ".rs", ".toml", ".json", ".yaml", ".yml", ".log", ".csv",
    ];
    has_separator || extensions.iter().any(|ext| lowercase.ends_with(ext))
}

fn revise_plan_for_tool_result(plan: &mut TaskPlan, tool_result: Option<&ToolResult>) -> bool {
    let Some(result) = tool_result else {
        return false;
    };
    if result.ok {
        return false;
    }

    plan.revise(
        "tool_execution",
        format!("工具执行未通过：{}", result.summary),
        "工具执行失败，切换为保守策略并提示用户修复后重试".to_string(),
    );
    plan.ensure_risk("工具执行失败导致上下文不足，需要用户确认后继续".to_string());
    plan.push_plan(
        "fallback_after_tool_failure",
        "记录工具失败原因，提供保守回答并建议重试",
        vec![PlanStep {
            id: scru128::new().to_string(),
            name: "fallback_response".to_string(),
            description: "保守输出当前结果并提示用户确认后重试".to_string(),
            status: PlanStepStatus::Completed,
        }],
    );
    plan.refresh_plan_statuses();
    true
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
