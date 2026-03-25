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
use crate::models_config::ModelsConfig;
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
    models_config: ModelsConfig,
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
            models_config: ModelsConfig::default(),
        }
    }

    pub fn with_models_config(mut self, config: ModelsConfig) -> Self {
        self.models_config = config;
        self
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
        // 设置当前线程的会话级工作目录
        let session_cwd = if session.cwd.is_empty() {
            None
        } else {
            let p = std::path::PathBuf::from(&session.cwd);
            if p.is_dir() { Some(p) } else { None }
        };
        crate::tool::set_session_cwd(session_cwd);

        // 累计 token 用量
        let mut accumulated_usage = TokenUsage::default();

        // 统一流程：所有请求直接进入 planning → execution → response
        // planning agent 自行判断是否需要工具调用，无需预分类

        // 规划阶段：流式 thinking 通过 on_stage_thinking 输出到系统消息
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
        // 构建上下文：先过滤掉执行阶段的 System 消息，再截取最近 N 条
        // 避免大量 System 消息占满 context 窗口挤掉用户/助手的真实对话
        let conversation_context: Vec<_> = session
            .messages
            .iter()
            .filter(|msg| {
                if msg.role != crate::session::MessageRole::System {
                    return true;
                }
                let c = msg.content.as_str();
                !(c.starts_with("工具执行")
                    || c.starts_with("LLM 输出")
                    || c.starts_with("Plan 执行总结")
                    || c.starts_with("检测到")
                    || c.starts_with("执行已取消"))
            })
            .cloned()
            .collect::<Vec<_>>();
        let context_start = conversation_context
            .len()
            .saturating_sub(self.context_limit);
        let context = conversation_context[context_start..].to_vec();

        let mut tool_results = Vec::new();

        // 执行阶段
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

        // response 阶段
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

        let cleaned_text = strip_tool_traces_from_response(&text);

        Ok(TurnExecution {
            assistant_message: cleaned_text,
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

/// 清理 LLM 响应中混入的工具执行 trace 文本。
///
/// 部分模型在 response 阶段会将 tool evidence 以原始 trace 格式复述到回答中，
/// 形如 "工具执行 [xxx]\n命令: ...\nok=... exit_code=... duration_ms=...\nsummary: ...\nstdout:\n..."
/// 这些内容应只存在于 System 消息中，不应出现在 assistant 回复里。
pub(crate) fn strip_tool_traces_from_response(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_trace_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // 检测工具 trace 块的起始行
        if trimmed.starts_with("工具执行") && trimmed.contains('[') && trimmed.contains(']') {
            in_trace_block = true;
            continue;
        }

        if in_trace_block {
            // trace 块内的特征行：跳过
            if trimmed.starts_with("命令:")
                || trimmed.starts_with("ok=")
                || trimmed.starts_with("summary:")
                || trimmed.starts_with("tool=")
                || trimmed.starts_with("stdout:")
                || trimmed.starts_with("stderr:")
                || (trimmed.starts_with("duration_ms="))
                || (trimmed.starts_with("exit_code="))
                || (trimmed.contains("ok=") && trimmed.contains("exit_code="))
            {
                continue;
            }
            // 空行也跳过（trace 块末尾的空行）
            if trimmed.is_empty() {
                continue;
            }
            // 非 trace 特征行，trace 块结束
            in_trace_block = false;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // 单行 trace 格式：整行包含工具执行关键字
        if trimmed.contains("工具执行")
            && trimmed.contains('[')
            && (trimmed.contains("ok=") || trimmed.contains("exit_code="))
        {
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

    // 清理多余连续空行
    let mut cleaned = String::with_capacity(result.len());
    let mut prev_empty = false;
    for line in result.lines() {
        if line.trim().is_empty() {
            if !prev_empty {
                cleaned.push('\n');
            }
            prev_empty = true;
        } else {
            cleaned.push_str(line);
            cleaned.push('\n');
            prev_empty = false;
        }
    }

    cleaned.trim().to_string()
}
