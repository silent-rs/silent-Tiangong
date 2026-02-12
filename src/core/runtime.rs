use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::core::model::{
    ModelClient, ModelRequest, ModelResponse, SingleProviderClient, TokenUsage,
};
use crate::core::planner::{TaskPlan, build_minimal_plan};
use crate::core::session::{Session, now_text};
use crate::core::tool::{PlaceholderToolExecutor, ToolCall, ToolExecutor, ToolName};

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
    pub usage: TokenUsage,
}

#[derive(Debug)]
pub struct RuntimeEngine {
    client: SingleProviderClient,
    tool_executor: PlaceholderToolExecutor,
    context_limit: usize,
}

impl RuntimeEngine {
    pub fn new(client: SingleProviderClient, context_limit: usize) -> Self {
        Self {
            client,
            tool_executor: PlaceholderToolExecutor,
            context_limit,
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

    pub fn execute_turn(&self, session: &Session, user_input: &str) -> Result<TurnExecution> {
        let plan = build_minimal_plan(user_input);
        let context = session.recent_messages(self.context_limit);
        let tool_result_summary = self.maybe_execute_tool(user_input)?;
        let prompt = if let Some(summary) = &tool_result_summary {
            format!("{user_input}\n\n工具预执行摘要：{summary}")
        } else {
            user_input.to_string()
        };
        let req = ModelRequest {
            session_title: session.title.clone(),
            user_input: prompt,
            context,
        };

        let ModelResponse { text, usage } = self.client.complete(&req)?;

        Ok(TurnExecution {
            assistant_message: text,
            plan,
            tool_result_summary,
            usage,
        })
    }

    fn maybe_execute_tool(&self, user_input: &str) -> Result<Option<String>> {
        if !(user_input.contains("目录")
            || user_input.contains("文件")
            || user_input.contains("命令"))
        {
            return Ok(None);
        }

        let call = if user_input.contains("目录") {
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
        Ok(Some(result.summary))
    }

    pub fn fallback_error_message(err: &anyhow::Error) -> String {
        format!("执行失败：{err}")
    }
}
