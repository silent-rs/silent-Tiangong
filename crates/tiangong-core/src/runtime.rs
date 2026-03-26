use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent_config::{AgentConfig, McpConfig};
use crate::agents::execution_mcp_agent::{
    McpFunctionTarget, execution_function_tools, execute_mcp_tool_call,
    resolve_mcp_tool_call_from_run_command,
};
use crate::agents::execution_tool_agent::build_tool_call_from_function;
use crate::model::{
    ModelClient, ModelFunctionCall, ModelRequest, ModelStreamChunk, SingleProviderClient, TokenUsage,
};
use crate::model::FunctionToolSpec;
use crate::models_config::{ModelCapability, ModelsConfig};
use crate::planner::TaskPlan;
use crate::session::{Message, MessageRole, Session, now_text};
use crate::tool::{LocalToolExecutor, ToolExecutionRecord, ToolExecutor, ToolResult};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    #[default]
    Idle,
    Planning,
    Executing,
    Completed,
    Failed,
}


#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

#[derive(Debug, Clone)]
pub struct LlmOutputRecord {
    pub stage: String,
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<String>,
    pub usage: TokenUsage,
}

pub use crate::agents::response_agent::VerifyExecutionRecord;

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

/// ReAct 循环的最大迭代次数
const MAX_REACT_ROUNDS: usize = 20;

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

    /// ReAct 单循环执行：思考 → 工具调用 → 观察 → ... → 最终回复
    #[allow(clippy::too_many_arguments)]
    pub fn execute_turn_with_streaming<F, P, L, T, S, G>(
        &self,
        session: &Session,
        user_input: &str,
        mut _on_plan_ready: P,
        mut on_chunk: F,
        mut on_llm_output: L,
        mut on_tool_result: T,
        mut _on_plan_execution_summary: S,
        mut _on_stage_thinking: G,
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

        let mut accumulated_usage = TokenUsage::default();

        // 构建对话上下文：过滤掉执行阶段的 System 消息，保留纯对话
        let conversation_context: Vec<_> = session
            .messages
            .iter()
            .filter(|msg| {
                if msg.role != MessageRole::System {
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

        // 准备工具定义
        let (function_tools, mcp_targets) =
            execution_function_tools(&self.agent_config.mcp);
        // 去掉 mark_step_completed 工具，ReAct 模式不需要手动完成信号
        let mut function_tools: Vec<_> = function_tools
            .into_iter()
            .filter(|t| t.name != "mark_step_completed")
            .collect();

        // 注入已配置的多媒体能力为工具
        tracing::info!(
            has_image = self.models_config.resolve_for_capability(ModelCapability::ImageGeneration).is_some(),
            has_video = self.models_config.resolve_for_capability(ModelCapability::VideoGeneration).is_some(),
            routing_keys = ?self.models_config.routing.keys().collect::<Vec<_>>(),
            "多媒体能力检查"
        );
        if self.models_config.resolve_for_capability(ModelCapability::ImageGeneration).is_some() {
            function_tools.push(FunctionToolSpec {
                name: "generate_image".to_string(),
                description: "根据文字描述生成图片。返回生成的图片 URL 或 base64 数据。".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "图片描述（英文效果更好）" }
                    },
                    "required": ["prompt"]
                }),
            });
        }
        if self.models_config.resolve_for_capability(ModelCapability::VideoGeneration).is_some() {
            function_tools.push(FunctionToolSpec {
                name: "generate_video".to_string(),
                description: "根据文字描述生成视频。返回生成的视频 URL。".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "视频内容描述（英文效果更好）" }
                    },
                    "required": ["prompt"]
                }),
            });
        }

        // 构建系统 prompt
        let system_prompt = build_react_system_prompt(user_input, &self.models_config);

        // 构建空 plan（兼容现有 TurnExecution 结构）
        let plan = TaskPlan {
            id: scru128::new().to_string(),
            objective: user_input.chars().take(50).collect::<String>(),
            summary: String::new(),
            plans: Vec::new(),
            risks: Vec::new(),
            skill_hints: Vec::new(),
            mcp_hints: Vec::new(),
            revisions: Vec::new(),
        };
        _on_plan_ready(&plan);

        // ReAct 循环
        let mut tool_results: Vec<ToolResult> = Vec::new();
        let mut loop_messages: Vec<Message> = Vec::new();
        let mut final_text = String::new();
        let mut final_reasoning = String::new();
        let mut total_output_chunks = 0usize;

        for round in 0..MAX_REACT_ROUNDS {
            let stage = format!("react-round-{}", round + 1);

            // 构建本轮请求
            let req = ModelRequest {
                session_title: session.title.clone(),
                user_input: if round == 0 {
                    system_prompt.clone()
                } else {
                    // 后续轮次：发送工具执行结果，让 agent 继续
                    "根据上面的工具执行结果继续处理。如果已经收集到足够信息，直接给出最终回复，不要再调用工具。".to_string()
                },
                context: {
                    let mut ctx = context.clone();
                    ctx.extend(loop_messages.clone());
                    ctx
                },
            };

            // 调用 LLM（带工具定义）
            // 中间轮次不流式到 assistant 消息，只有最终回复才流式推送
            let response = self.client.complete_with_functions_stream(
                &req,
                &function_tools,
                &mut |_delta: &ModelStreamChunk| {
                    // 暂不推送，等确定是否有工具调用后再决定
                },
            )?;

            accumulated_usage.accumulate(&response.usage);

            // 没有工具调用 → 最终回复，流式推送到 assistant 消息
            if response.tool_calls.is_empty() {
                final_text = response.text.clone();
                final_reasoning = response.reasoning_content.clone();
                // 一次性推送完整回复内容
                if !final_text.is_empty() || !final_reasoning.is_empty() {
                    on_chunk(&ModelStreamChunk {
                        content: final_text.clone(),
                        reasoning_content: final_reasoning.clone(),
                    });
                    total_output_chunks += 1;
                }
                break;
            }

            // 有工具调用 → 记录到执行过程（作为系统消息，按时间顺序排列）
            let tool_call_names: Vec<String> = response
                .tool_calls
                .iter()
                .map(|tc| tc.name.clone())
                .collect();
            let output = LlmOutputRecord {
                stage: stage.clone(),
                content: response.text.clone(),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: tool_call_names.clone(),
                usage: response.usage.clone(),
            };
            on_llm_output(&output);

            let mut round_feedback_parts: Vec<String> = Vec::new();

            // 记录 assistant 的工具调用意图到 loop_messages
            let assistant_text = if response.text.is_empty() {
                format!("[调用工具: {}]", tool_call_names.join(", "))
            } else {
                response.text.clone()
            };
            loop_messages.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::Assistant,
                content: assistant_text,
                reasoning_content: response.reasoning_content.clone(),
                created_at: now_text(),
            });

            for call in &response.tool_calls {
                let result = self.execute_tool_call(
                    call,
                    &mcp_targets,
                    &self.agent_config.mcp,
                );

                on_tool_result(&result);
                tool_results.push(result.clone());

                // 构建反馈信息
                let feedback = format!(
                    "工具 {} 执行{}：{}",
                    call.name,
                    if result.ok { "成功" } else { "失败" },
                    if result.stdout.len() > 2000 {
                        format!("{}...(截断)", &result.stdout[..2000])
                    } else if result.stdout.is_empty() {
                        result.summary.clone()
                    } else {
                        result.stdout.clone()
                    }
                );
                round_feedback_parts.push(feedback);
            }

            // 将工具执行结果作为 system message 加入 loop_messages
            let feedback_text = round_feedback_parts.join("\n\n");
            loop_messages.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: feedback_text,
                reasoning_content: String::new(),
                created_at: now_text(),
            });
        }

        // 如果循环结束仍未产生最终回复（达到 MAX_REACT_ROUNDS），做最后一次无工具调用
        if final_text.is_empty() {
            let req = ModelRequest {
                session_title: session.title.clone(),
                user_input: "请基于以上所有工具执行结果，直接给出最终回复。".to_string(),
                context: {
                    let mut ctx = context.clone();
                    ctx.extend(loop_messages.clone());
                    ctx
                },
            };
            let resp = if use_stream_mode() {
                self.client
                    .complete_stream_with_callback(&req, |delta| on_chunk(delta))?
            } else {
                let r = self.client.complete(&req)?;
                if !r.text.is_empty() {
                    on_chunk(&ModelStreamChunk {
                        content: r.text.clone(),
                        reasoning_content: r.reasoning_content.clone(),
                    });
                }
                r
            };
            accumulated_usage.accumulate(&resp.usage);
            final_text = resp.text;
            final_reasoning = resp.reasoning_content;
            total_output_chunks += 1;
        }

        let tool_result_summary = if tool_results.is_empty() {
            None
        } else {
            Some(format!(
                "{} 次工具调用，{} 成功，{} 失败",
                tool_results.len(),
                tool_results.iter().filter(|r| r.ok).count(),
                tool_results.iter().filter(|r| !r.ok).count(),
            ))
        };
        let tool_execution = tool_results
            .into_iter()
            .filter_map(|r| r.execution)
            .next_back();

        let cleaned_text = strip_tool_traces_from_response(&final_text);

        Ok(TurnExecution {
            assistant_message: cleaned_text,
            assistant_reasoning_content: final_reasoning,
            plan,
            tool_result_summary,
            tool_execution,
            verify_records: Vec::new(),
            output_mode: "stream".to_string(),
            output_chunk_count: total_output_chunks,
            usage: accumulated_usage,
        })
    }

    /// 执行单个工具调用（本地工具、MCP 工具或多媒体生成）
    fn execute_tool_call(
        &self,
        call: &ModelFunctionCall,
        mcp_targets: &HashMap<String, McpFunctionTarget>,
        mcp_config: &McpConfig,
    ) -> ToolResult {
        // 多媒体生成工具
        if call.name == "generate_image" {
            return self.handle_media_generation(call, ModelCapability::ImageGeneration);
        }
        if call.name == "generate_video" {
            return self.handle_media_generation(call, ModelCapability::VideoGeneration);
        }

        // 检查是否是 MCP 工具
        if let Some(target) = mcp_targets.get(&call.name) {
            return match execute_mcp_tool_call(call, target, mcp_config) {
                Ok(result) => result,
                Err(err) => ToolResult {
                    ok: false,
                    summary: format!("MCP工具调用失败：{err}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                    exit_code: 1,
                    execution: None,
                },
            };
        }

        // 检查 run_command 是否误用了 MCP 工具名
        if (call.name == "run_command" || call.name == "run_shell")
            && let Some((target, args)) =
                resolve_mcp_tool_call_from_run_command(call, mcp_targets, mcp_config)
        {
            return match crate::agents::execution_mcp_agent::execute_mcp_tool_call_with_args(
                &target, args, mcp_config,
            ) {
                Ok(result) => result,
                Err(err) => ToolResult {
                    ok: false,
                    summary: format!("MCP工具调用失败：{err}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                    exit_code: 1,
                    execution: None,
                },
            };
        }

        // 本地工具
        match build_tool_call_from_function(call) {
            Ok(tool_call) => match self.tool_executor.execute(&tool_call) {
                Ok(result) => result,
                Err(err) => ToolResult {
                    ok: false,
                    summary: format!("工具执行失败：{err}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                    exit_code: 1,
                    execution: None,
                },
            },
            Err(err) => ToolResult {
                ok: false,
                summary: format!("工具调用解析失败：{err}"),
                stdout: String::new(),
                stderr: err.to_string(),
                exit_code: 1,
                execution: None,
            },
        }
    }

    /// 处理多媒体生成工具调用（图片/视频）
    fn handle_media_generation(
        &self,
        call: &ModelFunctionCall,
        capability: ModelCapability,
    ) -> ToolResult {
        let prompt = call
            .arguments
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if prompt.is_empty() {
            return ToolResult {
                ok: false,
                summary: "缺少 prompt 参数".to_string(),
                stdout: String::new(),
                stderr: "prompt 不能为空".to_string(),
                exit_code: 1,
                execution: None,
            };
        }

        let resolved = match self.models_config.resolve_for_capability(capability) {
            Some(r) => r,
            None => {
                return ToolResult {
                    ok: false,
                    summary: format!("{}能力未配置", capability.display_name()),
                    stdout: String::new(),
                    stderr: "请在设置中配置对应的模型和路由".to_string(),
                    exit_code: 1,
                    execution: None,
                };
            }
        };

        let started = std::time::Instant::now();
        let tool_name = call.name.clone();

        // 使用 OpenAI 兼容 API 生成
        match capability {
            ModelCapability::ImageGeneration => {
                use tiangong_media::image::ImageGenerator;
                let generator = tiangong_media::openai_image::OpenAIImageGenerator::new(
                    resolved.api_key.clone(),
                    resolved.base_url.clone(),
                    resolved.model.clone(),
                );
                let request = tiangong_media::image::ImageGenRequest {
                    prompt,
                    negative_prompt: None,
                    width: 0,
                    height: 0,
                    model: Some(resolved.model.clone()),
                    style: None,
                    num_images: 1,
                };
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        return ToolResult {
                            ok: false,
                            summary: format!("运行时初始化失败：{e}"),
                            stdout: String::new(),
                            stderr: e.to_string(),
                            exit_code: 1,
                            execution: None,
                        };
                    }
                };
                let result = runtime.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        generator.generate(request),
                    )
                    .await
                });
                let duration_ms = started.elapsed().as_millis() as u64;
                match result {
                    Ok(Ok(resp)) => {
                        let mut parts = Vec::new();
                        for (i, img) in resp.images.iter().enumerate() {
                            if let Some(url) = &img.url {
                                parts.push(format!("![图片 {}]({})", i + 1, url));
                            } else if let Some(b64) = &img.b64_data {
                                parts.push(format!(
                                    "![图片 {}](data:image/png;base64,{})",
                                    i + 1,
                                    b64
                                ));
                            }
                        }
                        let output = parts.join("\n");
                        ToolResult {
                            ok: true,
                            summary: format!("图片生成成功（模型：{}）", resolved.model),
                            stdout: output,
                            stderr: String::new(),
                            exit_code: 0,
                            execution: Some(ToolExecutionRecord {
                                tool_name,
                                args: vec![],
                                duration_ms,
                                ok: true,
                                exit_code: 0,
                                summary: format!("图片生成成功（模型：{}）", resolved.model),
                            }),
                        }
                    }
                    Ok(Err(e)) => ToolResult {
                        ok: false,
                        summary: format!("图片生成失败：{e}"),
                        stdout: String::new(),
                        stderr: e.to_string(),
                        exit_code: 1,
                        execution: Some(ToolExecutionRecord {
                            tool_name,
                            args: vec![],
                            duration_ms,
                            ok: false,
                            exit_code: 1,
                            summary: format!("图片生成失败：{e}"),
                        }),
                    },
                    Err(_) => ToolResult {
                        ok: false,
                        summary: "图片生成超时（120秒）".to_string(),
                        stdout: String::new(),
                        stderr: "timeout".to_string(),
                        exit_code: 1,
                        execution: Some(ToolExecutionRecord {
                            tool_name,
                            args: vec![],
                            duration_ms,
                            ok: false,
                            exit_code: 1,
                            summary: "图片生成超时".to_string(),
                        }),
                    },
                }
            }
            ModelCapability::VideoGeneration => {
                use tiangong_media::openai_video::OpenAIVideoGenerator;
                use tiangong_media::video::{VideoGenStatus, VideoGenerator};

                let generator = OpenAIVideoGenerator::new(
                    resolved.api_key.clone(),
                    resolved.base_url.clone(),
                    resolved.model.clone(),
                );
                let request = tiangong_media::video::VideoGenRequest {
                    prompt,
                    duration: None,
                    resolution: None,
                    model: Some(resolved.model.clone()),
                    reference_image: None,
                };
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        return ToolResult {
                            ok: false,
                            summary: format!("运行时初始化失败：{e}"),
                            stdout: String::new(),
                            stderr: e.to_string(),
                            exit_code: 1,
                            execution: None,
                        };
                    }
                };

                // 提交任务
                let task = match runtime.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        generator.generate(request),
                    )
                    .await
                }) {
                    Ok(Ok(t)) => t,
                    Ok(Err(e)) => {
                        let duration_ms = started.elapsed().as_millis() as u64;
                        return ToolResult {
                            ok: false,
                            summary: format!("视频生成提交失败：{e}"),
                            stdout: String::new(),
                            stderr: e.to_string(),
                            exit_code: 1,
                            execution: Some(ToolExecutionRecord {
                                tool_name, args: vec![], duration_ms,
                                ok: false, exit_code: 1,
                                summary: format!("视频生成提交失败：{e}"),
                            }),
                        };
                    }
                    Err(_) => {
                        let duration_ms = started.elapsed().as_millis() as u64;
                        return ToolResult {
                            ok: false,
                            summary: "视频生成提交超时".to_string(),
                            stdout: String::new(),
                            stderr: "timeout".to_string(),
                            exit_code: 1,
                            execution: Some(ToolExecutionRecord {
                                tool_name, args: vec![], duration_ms,
                                ok: false, exit_code: 1,
                                summary: "视频生成提交超时".to_string(),
                            }),
                        };
                    }
                };

                // 同步返回的情况
                if let VideoGenStatus::Completed { video_url, .. } = &task.status {
                    let duration_ms = started.elapsed().as_millis() as u64;
                    return ToolResult {
                        ok: true,
                        summary: format!("视频生成成功（模型：{}）", resolved.model),
                        stdout: format!("[视频链接]({})", video_url),
                        stderr: String::new(),
                        exit_code: 0,
                        execution: Some(ToolExecutionRecord {
                            tool_name, args: vec![], duration_ms,
                            ok: true, exit_code: 0,
                            summary: format!("视频生成成功（模型：{}）", resolved.model),
                        }),
                    };
                }

                // 异步轮询（最多 5 分钟）
                let task_id = task.task_id;
                let poll_result = runtime.block_on(async {
                    let deadline =
                        tokio::time::Instant::now() + tokio::time::Duration::from_secs(300);
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        if tokio::time::Instant::now() > deadline {
                            return Err(anyhow::anyhow!("视频生成超时（5分钟）"));
                        }
                        match generator.query_status(&task_id).await {
                            Ok(VideoGenStatus::Completed { video_url, .. }) => {
                                return Ok(video_url);
                            }
                            Ok(VideoGenStatus::Failed { error }) => {
                                return Err(anyhow::anyhow!("视频生成失败：{error}"));
                            }
                            Ok(_) => continue,
                            Err(e) => {
                                tracing::warn!(%e, "视频状态查询出错，继续轮询");
                                continue;
                            }
                        }
                    }
                });

                let duration_ms = started.elapsed().as_millis() as u64;
                match poll_result {
                    Ok(video_url) => ToolResult {
                        ok: true,
                        summary: format!("视频生成成功（模型：{}）", resolved.model),
                        stdout: format!("[视频链接]({})", video_url),
                        stderr: String::new(),
                        exit_code: 0,
                        execution: Some(ToolExecutionRecord {
                            tool_name, args: vec![], duration_ms,
                            ok: true, exit_code: 0,
                            summary: format!("视频生成成功（模型：{}）", resolved.model),
                        }),
                    },
                    Err(e) => ToolResult {
                        ok: false,
                        summary: format!("视频生成失败：{e}"),
                        stdout: String::new(),
                        stderr: e.to_string(),
                        exit_code: 1,
                        execution: Some(ToolExecutionRecord {
                            tool_name, args: vec![], duration_ms,
                            ok: false, exit_code: 1,
                            summary: format!("视频生成失败：{e}"),
                        }),
                    },
                }
            }
            _ => ToolResult {
                ok: false,
                summary: format!("不支持的多媒体能力：{}", capability.display_name()),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
                execution: None,
            },
        }
    }

    pub fn fallback_error_message(err: &anyhow::Error) -> String {
        format!("执行失败：{err}")
    }
}

/// 构建 ReAct agent 的系统 prompt
fn build_react_system_prompt(user_input: &str, models_config: &ModelsConfig) -> String {
    use crate::models_config::ModelCapability;

    // 构建多媒体能力提示
    let mut media_hints = Vec::new();
    for cap in ModelCapability::media_capabilities() {
        if let Some(resolved) = models_config.resolve_for_capability(*cap) {
            media_hints.push(format!(
                "- {}：已配置（模型：{}），可通过 run_command 调用对应 API",
                cap.display_name(),
                resolved.model
            ));
        }
    }
    let media_section = if media_hints.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n已配置的多媒体能力（用户请求时应优先使用）：\n{}",
            media_hints.join("\n")
        )
    };

    format!(
"你是天工智能助手。你可以直接回答用户问题，也可以使用工具来完成任务。

规则：
1. 如果能直接回答（闲聊、知识问答等），直接回复，不要调用工具。
2. 如果需要文件操作、代码搜索、命令执行等，调用对应的工具。
3. 每次工具调用后会收到执行结果，根据结果决定下一步：继续调用工具或给出最终回复。
4. 回复时语言简洁，直接回答问题，不要说\"让我查看\"之类的过渡语。
5. 不要在回复中包含工具调用的原始痕迹（如 ok=、exit_code= 等元数据）。
6. 回复使用 Markdown 格式：代码和命令用代码块（```语言 ... ```）包裹，使用标题、列表等结构化排版。
7. 工具调用失败时必须如实告知用户失败原因，绝对不能虚构成功结果。{media_section}

用户输入：
{user_input}"
    )
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

/// 清理 LLM 响应中混入的工具执行 trace 文本
pub(crate) fn strip_tool_traces_from_response(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_trace_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("工具执行") && trimmed.contains('[') && trimmed.contains(']') {
            in_trace_block = true;
            continue;
        }

        if in_trace_block {
            if trimmed.starts_with("命令:")
                || trimmed.starts_with("ok=")
                || trimmed.starts_with("summary:")
                || trimmed.starts_with("tool=")
                || trimmed.starts_with("stdout:")
                || trimmed.starts_with("stderr:")
                || trimmed.starts_with("duration_ms=")
                || trimmed.starts_with("exit_code=")
                || (trimmed.contains("ok=") && trimmed.contains("exit_code="))
            {
                continue;
            }
            if trimmed.is_empty() {
                continue;
            }
            in_trace_block = false;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if trimmed.contains("工具执行")
            && trimmed.contains('[')
            && (trimmed.contains("ok=") || trimmed.contains("exit_code="))
        {
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

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
