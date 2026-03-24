use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent_config::AgentConfig;
use crate::agents::planning_agent;
use crate::agents::response_agent;
pub use crate::agents::response_agent::VerifyExecutionRecord;
use crate::execution::{
    execute_plan_with_execution_agent, recommend_verify_commands, run_verify_commands,
    summarize_tool_results,
};
use crate::model::{
    ModelClient, ModelRequest, ModelResponse, ModelStreamChunk, SingleProviderClient, TokenUsage,
};
use crate::planner::TaskPlan;
use crate::session::{Session, now_text};
use crate::tool::{LocalToolExecutor, ToolExecutionRecord, ToolResult};

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

pub use crate::execution::LlmOutputRecord;

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
            tool_executor: LocalToolExecutor::from_agent_config(&agent_config),
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
    pub fn execute_turn_with_streaming<F, P, L, T, S, G>(
        &self,
        session: &Session,
        user_input: &str,
        mut on_plan_ready: P,
        mut on_chunk: F,
        mut on_llm_output: L,
        mut on_tool_result: T,
        mut on_plan_execution_summary: S,
        mut on_stage_thinking: G,
    ) -> Result<TurnExecution>
    where
        P: FnMut(&TaskPlan),
        F: FnMut(&ModelStreamChunk),
        L: FnMut(&LlmOutputRecord),
        T: FnMut(&ToolResult),
        S: FnMut(&str),
        G: FnMut(&str, &ModelStreamChunk),
    {
        // 累计 token 用量
        let mut accumulated_usage = TokenUsage::default();

        // 意图分类：判断是否需要 planning
        if let Some(exec) = self.try_simple_chat(
            session,
            user_input,
            &mut on_chunk,
            &mut on_stage_thinking,
            &mut accumulated_usage,
        )? {
            return Ok(exec);
        }

        // 规划阶段：流式 thinking 通过 on_stage_thinking 输出到系统消息（"● Planning" 下方）
        let (mut plan, planning_output) = planning_agent::build_plan_with_agent_with_trace(
            &self.client,
            session,
            user_input,
            &self.agent_config,
            self.context_limit,
            &mut |delta: &ModelStreamChunk| {
                on_stage_thinking("planning-agent", delta);
            },
        );
        accumulated_usage.accumulate(&planning_output.usage);
        on_plan_ready(&plan);
        // 当规划输出有实质内容或有 token 消耗时，补充/更新系统消息记录规划结果
        if !planning_output.content.trim().is_empty()
            || !planning_output.reasoning_content.trim().is_empty()
            || planning_output.usage.total_tokens > 0
        {
            let output = LlmOutputRecord {
                stage: "planning-agent".to_string(),
                content: planning_output.content,
                reasoning_content: planning_output.reasoning_content,
                tool_calls: Vec::new(),
                usage: planning_output.usage,
            };
            on_llm_output(&output);
        }
        let context = session.recent_messages(self.context_limit);
        let mut tool_results = Vec::new();
        execute_plan_with_execution_agent(
            &self.client,
            &self.tool_executor,
            &self.agent_config.mcp,
            &mut plan,
            session,
            user_input,
            &context,
            &mut tool_results,
            &mut on_llm_output,
            &mut on_tool_result,
            &mut on_plan_execution_summary,
            &mut on_plan_ready,
            &mut on_stage_thinking,
            &mut accumulated_usage,
        )?;
        let verify_commands = recommend_verify_commands(user_input);
        let verify_records = run_verify_commands(&verify_commands);

        let reply_prompt = response_agent::build_grounded_response_prompt(
            user_input,
            &plan,
            &tool_results,
            &verify_records,
            &[],
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

        // 累加响应阶段的 token 用量
        accumulated_usage.accumulate(&usage);

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
            usage: accumulated_usage,
        })
    }

    /// 意图分类 + 简单对话快速回复
    ///
    /// 用轻量 LLM 调用判断用户输入是否为简单对话（问候、闲聊、知识问答等）。
    /// 如果是，直接生成回复并返回 Some(TurnExecution)，跳过 planning + execution。
    /// 如果需要工具/执行，返回 None，继续走完整流程。
    fn try_simple_chat(
        &self,
        session: &Session,
        user_input: &str,
        on_chunk: &mut dyn FnMut(&ModelStreamChunk),
        _on_stage_thinking: &mut dyn FnMut(&str, &ModelStreamChunk),
        accumulated_usage: &mut TokenUsage,
    ) -> Result<Option<TurnExecution>> {
        // 构建意图分类 prompt
        let classify_prompt = format!(
            "判断以下用户输入是否为【简单对话】（问候、闲聊、知识问答、翻译、解释概念等不需要执行工具或命令的请求）。\n\
            只回复 SIMPLE 或 COMPLEX，不要解释。\n\n\
            用户输入：{user_input}"
        );
        let classify_req = ModelRequest {
            session_title: format!("{} · intent-classify", session.title),
            user_input: classify_prompt,
            context: Vec::new(), // 分类不需要历史上下文
        };
        let classify_resp = self.client.complete(&classify_req);
        let is_simple = match &classify_resp {
            Ok(resp) => {
                accumulated_usage.accumulate(&resp.usage);
                resp.text.trim().to_uppercase().contains("SIMPLE")
            }
            Err(_) => false, // 分类失败则走完整流程
        };

        if !is_simple {
            return Ok(None);
        }

        // 简单对话：直接用对话模式回复
        let context = session.recent_messages(self.context_limit);
        let req = ModelRequest {
            session_title: session.title.clone(),
            user_input: user_input.to_string(),
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
                    if !resp.text.is_empty() {
                        on_chunk(&ModelStreamChunk {
                            content: resp.text.clone(),
                            reasoning_content: resp.reasoning_content.clone(),
                        });
                    }
                    resp
                }
            }
        } else {
            let resp = self.client.complete(&req)?;
            if !resp.text.is_empty() {
                on_chunk(&ModelStreamChunk {
                    content: resp.text.clone(),
                    reasoning_content: resp.reasoning_content.clone(),
                });
            }
            resp
        };

        accumulated_usage.accumulate(&usage);

        // 构建空的 plan（标记为简单对话）
        let plan = TaskPlan {
            id: scru128::new().to_string(),
            objective: "简单对话".to_string(),
            summary: format!("直接回复用户输入：{}", user_input),
            plans: Vec::new(),
            risks: Vec::new(),
            skill_hints: Vec::new(),
            mcp_hints: Vec::new(),
            revisions: Vec::new(),
        };

        Ok(Some(TurnExecution {
            assistant_message: text,
            assistant_reasoning_content: reasoning_content,
            plan,
            tool_result_summary: None,
            tool_execution: None,
            verify_records: Vec::new(),
            output_mode,
            output_chunk_count,
            usage: accumulated_usage.clone(),
        }))
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
