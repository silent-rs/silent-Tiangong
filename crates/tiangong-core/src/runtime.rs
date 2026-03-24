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
use crate::models_config::{ModelCapability, ModelsConfig};
use tiangong_media::image::ImageGenerator;
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
        // 累计 token 用量
        let mut accumulated_usage = TokenUsage::default();

        // 统一意图分类：一次 LLM 调用判断请求类型
        let intent = self.classify_intent(session, user_input, &mut accumulated_usage);
        match intent.as_str() {
            "SIMPLE" => {
                if let Some(exec) = self.handle_simple_chat(
                    session, user_input, &mut on_chunk, &mut accumulated_usage,
                )? {
                    return Ok(exec);
                }
            }
            "IMAGE" => {
                if let Some(exec) = self.handle_image_generation(
                    user_input, &mut on_chunk, &mut accumulated_usage,
                )? {
                    return Ok(exec);
                }
            }
            _ => {} // COMPLEX 或分类失败，继续走完整 planning 流程
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

    /// 统一意图分类：一次 LLM 调用判断请求类型
    ///
    /// 根据 ModelsConfig 中已配置的能力路由自动加载分类选项。
    fn classify_intent(
        &self,
        session: &Session,
        user_input: &str,
        accumulated_usage: &mut TokenUsage,
    ) -> String {
        // 固定选项：简单对话 + 复杂任务
        let mut options = vec![
            format!("SIMPLE - {}", ModelCapability::Chat.intent_description()),
            "COMPLEX - 复杂任务（需要执行工具、命令、代码操作、文件读写等）".to_string(),
        ];

        // 动态加载：遍历所有多媒体能力，已配置 routing 的自动加入
        let mut active_labels = Vec::new();
        for cap in ModelCapability::media_capabilities() {
            if self.models_config.resolve_for_capability(*cap).is_some() {
                let label = cap.intent_label();
                options.push(format!("{} - {}", label, cap.intent_description()));
                active_labels.push(label.to_string());
            }
        }

        let all_labels: Vec<&str> = options.iter().map(|o| o.split(" - ").next().unwrap_or("")).collect();
        let labels_hint = all_labels.join("、");
        let options_text = options.join("\n");

        let classify_prompt = format!(
            "判断以下用户输入属于哪种类型，只回复对应的标签（{labels_hint} 之一），不要解释。\n\n\
            可选类型：\n{options_text}\n\n\
            用户输入：{user_input}"
        );

        let classify_req = ModelRequest {
            session_title: format!("{} · intent-classify", session.title),
            user_input: classify_prompt,
            context: Vec::new(),
        };

        match self.client.complete(&classify_req) {
            Ok(resp) => {
                accumulated_usage.accumulate(&resp.usage);
                let text = resp.text.trim().to_uppercase();
                // 按优先级匹配：先检查多媒体能力标签，再检查 SIMPLE，最后 COMPLEX
                for label in &active_labels {
                    if text.contains(label.as_str()) {
                        return label.clone();
                    }
                }
                if text.contains("SIMPLE") {
                    "SIMPLE".to_string()
                } else {
                    "COMPLEX".to_string()
                }
            }
            Err(_) => "COMPLEX".to_string(),
        }
    }

    /// 简单对话快速回复
    fn handle_simple_chat(
        &self,
        session: &Session,
        user_input: &str,
        on_chunk: &mut dyn FnMut(&ModelStreamChunk),
        accumulated_usage: &mut TokenUsage,
    ) -> Result<Option<TurnExecution>> {
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

    /// 图片生成快速路径
    fn handle_image_generation(
        &self,
        user_input: &str,
        on_chunk: &mut dyn FnMut(&ModelStreamChunk),
        accumulated_usage: &mut TokenUsage,
    ) -> Result<Option<TurnExecution>> {
        let resolved = match self.models_config.resolve_for_capability(ModelCapability::ImageGeneration) {
            Some(r) => r,
            None => return Ok(None),
        };

        let prompt = user_input.to_string();

        // 调用图片生成 API
        let generator = tiangong_media::openai_image::OpenAIImageGenerator::new(
            resolved.api_key.clone(),
            resolved.base_url.trim_end_matches('/').to_string(),
            resolved.model.clone(),
        );

        let request = tiangong_media::image::ImageGenRequest {
            prompt: prompt.clone(),
            negative_prompt: None,
            width: 1024,
            height: 1024,
            model: Some(resolved.model.clone()),
            style: None,
            num_images: 1,
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow::anyhow!("初始化异步运行时失败：{e}"))?;

        let result = runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(120),
                generator.generate(request),
            ).await
        });

        let response = match result {
            Ok(Ok(resp)) => resp,
            Ok(Err(err)) => {
                // 图片生成失败，回退给 chat 模型处理
                let error_msg = format!("图片生成失败：{err}");
                on_chunk(&ModelStreamChunk {
                    content: error_msg.clone(),
                    reasoning_content: String::new(),
                });
                let plan = TaskPlan {
                    id: scru128::new().to_string(),
                    objective: "图片生成".to_string(),
                    summary: format!("尝试生成图片但失败：{err}"),
                    plans: Vec::new(),
                    risks: Vec::new(),
                    skill_hints: Vec::new(),
                    mcp_hints: Vec::new(),
                    revisions: Vec::new(),
                };
                return Ok(Some(TurnExecution {
                    assistant_message: error_msg,
                    assistant_reasoning_content: String::new(),
                    plan,
                    tool_result_summary: None,
                    tool_execution: None,
                    verify_records: Vec::new(),
                    output_mode: "sync".to_string(),
                    output_chunk_count: 1,
                    usage: accumulated_usage.clone(),
                }));
            }
            Err(_) => {
                let error_msg = "图片生成超时（120秒）".to_string();
                on_chunk(&ModelStreamChunk {
                    content: error_msg.clone(),
                    reasoning_content: String::new(),
                });
                let plan = TaskPlan {
                    id: scru128::new().to_string(),
                    objective: "图片生成".to_string(),
                    summary: "图片生成超时".to_string(),
                    plans: Vec::new(),
                    risks: Vec::new(),
                    skill_hints: Vec::new(),
                    mcp_hints: Vec::new(),
                    revisions: Vec::new(),
                };
                return Ok(Some(TurnExecution {
                    assistant_message: error_msg,
                    assistant_reasoning_content: String::new(),
                    plan,
                    tool_result_summary: None,
                    tool_execution: None,
                    verify_records: Vec::new(),
                    output_mode: "sync".to_string(),
                    output_chunk_count: 1,
                    usage: accumulated_usage.clone(),
                }));
            }
        };

        // 构建回复：包含图片 URL 或 base64
        let mut reply_parts = Vec::new();
        reply_parts.push(format!("已为您生成图片（模型：{}）：\n", resolved.model));
        for (i, img) in response.images.iter().enumerate() {
            if let Some(url) = &img.url {
                reply_parts.push(format!("![生成的图片 {}]({})\n", i + 1, url));
            } else if let Some(b64) = &img.b64_data {
                reply_parts.push(format!("![生成的图片 {}](data:image/png;base64,{})\n", i + 1, b64));
            }
            if let Some(revised) = &img.revised_prompt {
                reply_parts.push(format!("优化后的提示词：{}\n", revised));
            }
        }
        let reply = reply_parts.join("");

        on_chunk(&ModelStreamChunk {
            content: reply.clone(),
            reasoning_content: String::new(),
        });

        let plan = TaskPlan {
            id: scru128::new().to_string(),
            objective: "图片生成".to_string(),
            summary: format!("使用 {} 生成图片", resolved.model),
            plans: Vec::new(),
            risks: Vec::new(),
            skill_hints: Vec::new(),
            mcp_hints: Vec::new(),
            revisions: Vec::new(),
        };

        Ok(Some(TurnExecution {
            assistant_message: reply,
            assistant_reasoning_content: String::new(),
            plan,
            tool_result_summary: None,
            tool_execution: None,
            verify_records: Vec::new(),
            output_mode: "stream".to_string(),
            output_chunk_count: 1,
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
