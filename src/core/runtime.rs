use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::core::agent_config::AgentConfig;
use crate::core::mcp::collect_mcp_context;
use crate::core::model::{
    ModelClient, ModelRequest, ModelResponse, SingleProviderClient, TokenUsage,
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
    pub plan: TaskPlan,
    pub tool_result_summary: Option<String>,
    pub tool_execution: Option<ToolExecutionRecord>,
    pub output_mode: String,
    pub output_chunk_count: usize,
    pub usage: TokenUsage,
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
        F: FnMut(&str),
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
                    on_chunk(&resp.text);
                    resp
                }
            }
        } else {
            let resp = self.client.complete(&req)?;
            on_chunk(&resp.text);
            resp
        };

        Ok(TurnExecution {
            assistant_message: text,
            plan,
            tool_result_summary,
            tool_execution: tool_result.and_then(|result| result.execution),
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
