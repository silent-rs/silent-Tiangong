use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent_config::{AgentConfig, McpConfig};
use crate::app_state::ManagementCommand;
use crate::agents::execution_mcp_agent::{
    McpFunctionTarget, execution_function_tools, execute_mcp_tool_call,
    resolve_mcp_tool_call_from_run_command,
};
use crate::agents::execution_tool_agent::build_tool_call_from_function;
use crate::context::compressor::compress_loop_messages;
use crate::context::organizer::ContextOrganizer;
use crate::model::{
    ModelClient, ModelFunctionCall, ModelRequest, ModelStreamChunk, SingleProviderClient, TokenUsage,
};
use crate::model::FunctionToolSpec;
use crate::models_config::{ModelCapability, ModelsConfig};
use crate::planner::TaskPlan;
use crate::session::{Message, MessageRole, Session, now_text};
use crate::tool::{LocalToolExecutor, ToolExecutionRecord, ToolExecutor, ToolResult};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    #[default]
    Idle,
    Planning,
    Executing,
    /// 等待用户审批（高风险工具操作）
    WaitingApproval,
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
    pub context_limit: usize,
    agent_config: AgentConfig,
    models_config: ModelsConfig,
    permission_gate: crate::permission::PermissionGate,
}

impl RuntimeEngine {
    pub fn new(
        client: SingleProviderClient,
        context_limit: usize,
        agent_config: AgentConfig,
    ) -> Self {
        let permission_gate = crate::permission::PermissionGate::new(
            crate::permission::PermissionPolicy {
                trust_mode: agent_config.trust_mode,
                ..Default::default()
            },
        );
        Self {
            client,
            tool_executor: LocalToolExecutor::from_agent_config(&agent_config),
            context_limit,
            agent_config,
            models_config: ModelsConfig::default(),
            permission_gate,
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
    pub fn execute_turn_with_streaming<F, P, L, T, S, G, M>(
        &self,
        session: &Session,
        user_input: &str,
        mut _on_plan_ready: P,
        mut on_chunk: F,
        mut on_llm_output: L,
        mut on_tool_result: T,
        mut _on_plan_execution_summary: S,
        mut _on_stage_thinking: G,
        mut on_management_cmd: M,
    ) -> Result<TurnExecution>
    where
        P: FnMut(&TaskPlan),
        F: FnMut(&ModelStreamChunk),
        L: FnMut(&LlmOutputRecord),
        T: FnMut(&ToolResult),
        S: FnMut(&str),
        G: FnMut(&str, &ModelStreamChunk),
        M: FnMut(ManagementCommand) + Send,
    {
        // === 快速路径：简单对话跳过工具注入，节省 token ===
        if Self::is_simple_chat(user_input) {
            let organizer = ContextOrganizer::new(self.context_limit)
                .with_keep_recent_turns(6);
            let context = organizer.build_context(session);

            let req = ModelRequest {
                session_title: session.title.clone(),
                user_input: user_input.to_string(),
                context,
            };

            let resp = if use_stream_mode() {
                self.client.complete_stream_with_callback(&req, |delta| on_chunk(delta))?
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

            let cleaned = strip_tool_traces_from_response(&resp.text);
            return Ok(TurnExecution {
                assistant_message: cleaned,
                assistant_reasoning_content: resp.reasoning_content,
                plan: TaskPlan {
                    id: scru128::new().to_string(),
                    objective: user_input.chars().take(50).collect::<String>(),
                    ..Default::default()
                },
                tool_result_summary: None,
                tool_execution: None,
                verify_records: Vec::new(),
                output_mode: "stream".to_string(),
                output_chunk_count: 1,
                usage: resp.usage,
            });
        }

        // 设置当前线程的会话级工作目录
        let session_cwd = if session.cwd.is_empty() {
            None
        } else {
            let p = std::path::PathBuf::from(&session.cwd);
            if p.is_dir() { Some(p) } else { None }
        };
        crate::tool::set_session_cwd(session_cwd.clone());

        let mut accumulated_usage = TokenUsage::default();

        // 构建对话上下文：摘要 + 最近消息 + 过滤执行痕迹
        // 压缩（摘要更新）已在 turn 启动前完成并持久化到 session
        let organizer = ContextOrganizer::new(self.context_limit)
            .with_keep_recent_turns(6);
        let context = organizer.build_context(session);

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
                        "prompt": { "type": "string", "description": "图片描述（英文效果更好）" },
                        "width": { "type": "integer", "description": "图片宽度（像素，可选，0 表示默认）" },
                        "height": { "type": "integer", "description": "图片高度（像素，可选，0 表示默认）" },
                        "style": { "type": "string", "description": "风格（如 vivid、natural，可选）" }
                    },
                    "required": ["prompt"]
                }),
            });
        }
        // 视频生成暂通过 skill 机制处理，不注入内置工具

        // 语音合成（TTS）
        if self.models_config.resolve_for_capability(ModelCapability::Tts).is_some() {
            function_tools.push(FunctionToolSpec {
                name: "text_to_speech".to_string(),
                description: "将文本转换为语音音频文件，保存到指定路径并返回文件路径。".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "待合成的文本" },
                        "voice": { "type": "string", "description": "音色（如 alloy、echo、fable、onyx、nova、shimmer，可选）" },
                        "speed": { "type": "number", "description": "语速（0.5~2.0，可选，默认 1.0）" },
                        "output_path": { "type": "string", "description": "输出文件路径（可选，默认自动生成）" }
                    },
                    "required": ["text"]
                }),
            });
        }

        // 语音识别（STT）
        if self.models_config.resolve_for_capability(ModelCapability::Stt).is_some() {
            function_tools.push(FunctionToolSpec {
                name: "speech_to_text".to_string(),
                description: "将音频文件转录为文本。支持 mp3、wav、ogg 等格式。".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "音频文件路径" },
                        "language": { "type": "string", "description": "语言提示（如 zh、en，可选）" }
                    },
                    "required": ["file_path"]
                }),
            });
        }

        // 注入 get_skill_detail 工具（已安装 skill 时可用）
        if self.agent_config.skills.installed.iter().any(|s| s.enabled) {
            function_tools.push(FunctionToolSpec {
                name: "get_skill_detail".to_string(),
                description: "获取已安装 Skill 的完整使用说明，返回 SKILL.md 内容和调用命令".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "skill_id": { "type": "string", "description": "Skill ID（从已安装列表中选择）" }
                    },
                    "required": ["skill_id"]
                }),
            });
        }

        // 后台任务管理工具
        function_tools.push(FunctionToolSpec {
            name: "spawn_task".to_string(),
            description: "在后台启动长时间运行的命令，立即返回 task_id。适用于编译、下载、视频生成等耗时操作。".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "任务名称（用于显示）" },
                    "cmd": { "type": "string", "description": "命令名" },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "命令参数" },
                    "cwd": { "type": "string", "description": "工作目录（可选）" }
                },
                "required": ["name", "cmd"]
            }),
        });
        function_tools.push(FunctionToolSpec {
            name: "query_task".to_string(),
            description: "查询后台任务的状态和输出结果".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "任务 ID" }
                },
                "required": ["task_id"]
            }),
        });
        function_tools.push(FunctionToolSpec {
            name: "list_tasks".to_string(),
            description: "列出所有后台任务及其状态".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        });
        function_tools.push(FunctionToolSpec {
            name: "cancel_task".to_string(),
            description: "取消一个正在运行的后台任务".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "任务 ID" }
                },
                "required": ["task_id"]
            }),
        });
        function_tools.push(FunctionToolSpec {
            name: "wait_tasks".to_string(),
            description: "等待多个后台任务全部完成（spawn+join 模式）。适用于先用 spawn_task 启动多个并行任务，然后一次性等待所有结果。".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "要等待的任务 ID 列表"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "超时毫秒数，0 表示无限等待（默认 0）"
                    }
                },
                "required": ["task_ids"]
            }),
        });

        // MCP/Skill 管理工具（始终可用）
        function_tools.push(FunctionToolSpec {
            name: "register_mcp_server".to_string(),
            description: "注册一个新的 MCP 服务器。注册后服务器将自动启动并加载工具。".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "服务器名称（唯一标识）" },
                    "command": { "type": "string", "description": "启动命令（如 uvx、npx 的完整路径）" },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "命令参数" },
                    "env": { "type": "object", "description": "环境变量（键值对）" },
                    "transport": { "type": "string", "description": "传输方式：stdio（默认）或 http" },
                    "endpoint": { "type": "string", "description": "HTTP 端点 URL（transport=http 时必填）" }
                },
                "required": ["name", "command"]
            }),
        });
        function_tools.push(FunctionToolSpec {
            name: "remove_mcp_server".to_string(),
            description: "移除一个已注册的 MCP 服务器".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "服务器名称" }
                },
                "required": ["name"]
            }),
        });
        function_tools.push(FunctionToolSpec {
            name: "set_mcp_enabled".to_string(),
            description: "启用或禁用一个 MCP 服务器".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "服务器名称" },
                    "enabled": { "type": "boolean", "description": "是否启用" }
                },
                "required": ["name", "enabled"]
            }),
        });
        function_tools.push(FunctionToolSpec {
            name: "install_skill".to_string(),
            description: "从本地路径安装一个 Skill。路径应指向包含 skill.toml 和 SKILL.md 的目录。".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Skill 目录路径" },
                    "enabled": { "type": "boolean", "description": "安装后是否启用（默认 true）" }
                },
                "required": ["path"]
            }),
        });
        function_tools.push(FunctionToolSpec {
            name: "remove_skill".to_string(),
            description: "卸载一个已安装的 Skill".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Skill ID" }
                },
                "required": ["id"]
            }),
        });
        function_tools.push(FunctionToolSpec {
            name: "set_skill_enabled".to_string(),
            description: "启用或禁用一个已安装的 Skill".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Skill ID" },
                    "enabled": { "type": "boolean", "description": "是否启用" }
                },
                "required": ["id", "enabled"]
            }),
        });

        // 构建系统 prompt
        let system_prompt =
            build_react_system_prompt(user_input, &self.models_config, &self.agent_config);

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

            // 并发执行所有工具调用（子线程独立处理，全部返回后继续）
            // 管理工具需要 &mut 回调，先提取处理，其余工具并发执行
            let mut mgmt_results: Vec<(String, ToolResult)> = Vec::new();
            let mut other_calls: Vec<&ModelFunctionCall> = Vec::new();
            for call in &response.tool_calls {
                if let Some(result) = Self::handle_management_tool(call, &mut on_management_cmd) {
                    mgmt_results.push((call.name.clone(), result));
                } else {
                    other_calls.push(call);
                }
            }

            let mut call_results: Vec<(String, ToolResult)> = mgmt_results;
            if other_calls.len() == 1 {
                // 单个工具调用：直接执行，避免线程开销
                let call = other_calls[0];
                let result = self.execute_tool_call(call, &mcp_targets, &self.agent_config.mcp);
                call_results.push((call.name.clone(), result));
            } else if !other_calls.is_empty() {
                // 多个工具调用：并发执行
                let other_results: Vec<(String, ToolResult)> = std::thread::scope(|scope| {
                    let handles: Vec<_> = other_calls
                        .iter()
                        .map(|call| {
                            let mcp_targets = &mcp_targets;
                            let mcp_config = &self.agent_config.mcp;
                            let name = call.name.clone();
                            // 捕获会话 CWD，传递到子线程
                            let thread_cwd = session_cwd.clone();
                            scope.spawn(move || {
                                crate::tool::set_session_cwd(thread_cwd);
                                let result = self.execute_tool_call(call, mcp_targets, mcp_config);
                                (name, result)
                            })
                        })
                        .collect();

                    handles
                        .into_iter()
                        .map(|h| h.join().unwrap_or_else(|_| {
                            ("unknown".to_string(), ToolResult {
                                ok: false,
                                summary: "工具执行线程 panic".to_string(),
                                stdout: String::new(),
                                stderr: "thread panicked".to_string(),
                                exit_code: 1,
                                execution: None,
                            })
                        }))
                        .collect()
                });
                call_results.extend(other_results);
            }

            let mut round_feedback_parts: Vec<String> = Vec::new();
            for (call_name, result) in call_results {
                on_tool_result(&result);
                tool_results.push(result.clone());

                let feedback = format!(
                    "工具 {} 执行{}：{}",
                    call_name,
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

            // 基于 API 返回的精确 prompt_tokens 判断是否需要压缩 loop_messages
            // 当本轮输入 token 超过 context_limit 的 70% 时，压缩早期轮次
            let prompt_tokens = response.usage.prompt_tokens;
            if organizer.needs_compression(prompt_tokens) {
                tracing::info!(
                    prompt_tokens,
                    threshold = organizer.token_threshold(),
                    "prompt_tokens 超过阈值，压缩 loop_messages"
                );
                // 保留最近 3 轮完整信息
                match compress_loop_messages(&loop_messages, 3, &self.client) {
                    Ok(compressed) => loop_messages = compressed,
                    Err(err) => tracing::warn!("loop_messages 压缩失败：{err}"),
                }
            }
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

    /// 执行单个工具调用（本地工具、MCP 工具、多媒体生成或后台任务）
    fn execute_tool_call(
        &self,
        call: &ModelFunctionCall,
        mcp_targets: &HashMap<String, McpFunctionTarget>,
        mcp_config: &McpConfig,
    ) -> ToolResult {
        // 权限检查
        use crate::permission::PermissionDecision;
        match self.permission_gate.check(&call.name) {
            PermissionDecision::Approved => {}
            PermissionDecision::Denied { reason } => {
                return ToolResult {
                    ok: false,
                    summary: format!("权限拒绝：{reason}"),
                    stdout: String::new(),
                    stderr: reason,
                    exit_code: 1,
                    execution: None,
                };
            }
            PermissionDecision::NeedsApproval { request_id } => {
                // Phase 3 中将改为暂停等待审批，当前在非 FullTrust 模式下记录日志并放行
                tracing::info!(
                    "权限审批请求 {request_id}：工具 {} 需要用户确认（当前自动放行）",
                    call.name
                );
            }
        }

        // 后台任务管理
        if let Some(result) = self.handle_background_task(call) {
            return result;
        }
        // Skill 详情查询
        if call.name == "get_skill_detail" {
            return self.handle_get_skill_detail(call);
        }

        // 多媒体工具
        if call.name == "generate_image" {
            return self.handle_media_generation(call, ModelCapability::ImageGeneration);
        }
        if call.name == "text_to_speech" {
            return self.handle_tts(call);
        }
        if call.name == "speech_to_text" {
            return self.handle_stt(call);
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

    /// 处理后台任务工具调用
    fn handle_background_task(&self, call: &ModelFunctionCall) -> Option<ToolResult> {
        use crate::tool::background_task::{task_registry, TaskStatus};

        match call.name.as_str() {
            "spawn_task" => {
                let name = call.arguments.get("name").and_then(|v| v.as_str()).unwrap_or("task").to_string();
                let cmd = call.arguments.get("cmd").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let args: Vec<String> = call.arguments.get("args")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(String::from).collect())
                    .unwrap_or_default();
                let cwd = call.arguments.get("cwd").and_then(|v| v.as_str()).map(String::from);

                if cmd.is_empty() {
                    return Some(ToolResult {
                        ok: false, summary: "缺少 cmd 参数".to_string(),
                        stdout: String::new(), stderr: String::new(), exit_code: 1, execution: None,
                    });
                }

                let env = self.tool_executor.runtime_env().iter()
                    .map(|(k, v)| (k.clone(), v.clone())).collect();

                match task_registry().lock() {
                    Ok(mut reg) => match reg.spawn(name, cmd, args, cwd, env) {
                        Ok(task_id) => Some(ToolResult {
                            ok: true,
                            summary: format!("后台任务已启动，task_id={task_id}"),
                            stdout: serde_json::json!({"task_id": task_id}).to_string(),
                            stderr: String::new(), exit_code: 0, execution: None,
                        }),
                        Err(e) => Some(ToolResult {
                            ok: false, summary: format!("启动后台任务失败：{e}"),
                            stdout: String::new(), stderr: e, exit_code: 1, execution: None,
                        }),
                    },
                    Err(e) => Some(ToolResult {
                        ok: false, summary: format!("任务注册表锁失败：{e}"),
                        stdout: String::new(), stderr: e.to_string(), exit_code: 1, execution: None,
                    }),
                }
            }
            "query_task" => {
                let task_id = call.arguments.get("task_id").and_then(|v| v.as_str()).unwrap_or_default();
                match task_registry().lock() {
                    Ok(mut reg) => match reg.query(task_id) {
                        Some(info) => {
                            let status_text = match &info.status {
                                TaskStatus::Running => "running".to_string(),
                                TaskStatus::Completed { exit_code } => format!("completed (exit_code={exit_code})"),
                                TaskStatus::Failed { error } => format!("failed: {error}"),
                                TaskStatus::Cancelled => "cancelled".to_string(),
                            };
                            Some(ToolResult {
                                ok: true,
                                summary: format!("任务 {} 状态：{}", info.name, status_text),
                                stdout: serde_json::to_string_pretty(&info).unwrap_or_default(),
                                stderr: String::new(), exit_code: 0, execution: None,
                            })
                        }
                        None => Some(ToolResult {
                            ok: false, summary: format!("未找到任务：{task_id}"),
                            stdout: String::new(), stderr: String::new(), exit_code: 1, execution: None,
                        }),
                    },
                    Err(e) => Some(ToolResult {
                        ok: false, summary: e.to_string(),
                        stdout: String::new(), stderr: String::new(), exit_code: 1, execution: None,
                    }),
                }
            }
            "list_tasks" => {
                match task_registry().lock() {
                    Ok(mut reg) => {
                        let tasks = reg.list();
                        Some(ToolResult {
                            ok: true,
                            summary: format!("{} 个后台任务", tasks.len()),
                            stdout: serde_json::to_string_pretty(&tasks).unwrap_or_default(),
                            stderr: String::new(), exit_code: 0, execution: None,
                        })
                    }
                    Err(e) => Some(ToolResult {
                        ok: false, summary: e.to_string(),
                        stdout: String::new(), stderr: String::new(), exit_code: 1, execution: None,
                    }),
                }
            }
            "cancel_task" => {
                let task_id = call.arguments.get("task_id").and_then(|v| v.as_str()).unwrap_or_default();
                match task_registry().lock() {
                    Ok(mut reg) => match reg.cancel(task_id) {
                        Some(info) => Some(ToolResult {
                            ok: true,
                            summary: format!("任务 {} 已取消", info.name),
                            stdout: serde_json::to_string_pretty(&info).unwrap_or_default(),
                            stderr: String::new(), exit_code: 0, execution: None,
                        }),
                        None => Some(ToolResult {
                            ok: false, summary: format!("未找到任务：{task_id}"),
                            stdout: String::new(), stderr: String::new(), exit_code: 1, execution: None,
                        }),
                    },
                    Err(e) => Some(ToolResult {
                        ok: false, summary: e.to_string(),
                        stdout: String::new(), stderr: String::new(), exit_code: 1, execution: None,
                    }),
                }
            }
            "wait_tasks" => {
                let task_ids: Vec<String> = call.arguments.get("task_ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(String::from).collect())
                    .unwrap_or_default();
                let timeout_ms = call.arguments.get("timeout_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                if task_ids.is_empty() {
                    return Some(ToolResult {
                        ok: false, summary: "缺少 task_ids 参数".to_string(),
                        stdout: String::new(), stderr: String::new(), exit_code: 1, execution: None,
                    });
                }

                let results = crate::tool::background_task::wait_tasks(task_ids, timeout_ms);
                let all_ok = results.iter().all(|r| matches!(r.status, TaskStatus::Completed { exit_code } if exit_code == 0));
                let running_count = results.iter().filter(|r| matches!(r.status, TaskStatus::Running)).count();
                let summary = if running_count > 0 {
                    format!("{} 个任务完成，{} 个仍在运行（超时）", results.len() - running_count, running_count)
                } else {
                    format!("{} 个任务全部完成", results.len())
                };

                Some(ToolResult {
                    ok: all_ok,
                    summary,
                    stdout: serde_json::to_string_pretty(&results).unwrap_or_default(),
                    stderr: String::new(),
                    exit_code: if all_ok { 0 } else { 1 },
                    execution: None,
                })
            }
            _ => None, // 不是后台任务工具
        }
    }

    /// 获取 skill 完整说明
    /// 处理 MCP/Skill 管理工具调用，返回 Some 表示已处理
    fn handle_management_tool(
        call: &ModelFunctionCall,
        on_cmd: &mut impl FnMut(ManagementCommand),
    ) -> Option<ToolResult> {
        let cmd = match call.name.as_str() {
            "register_mcp_server" => {
                let name = call.arguments.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let command = call.arguments.get("command").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                if name.is_empty() || command.is_empty() {
                    return Some(ToolResult {
                        ok: false, summary: "name 和 command 为必填参数".to_string(),
                        stdout: String::new(), stderr: String::new(), exit_code: 1, execution: None,
                    });
                }
                let args: Vec<String> = call.arguments.get("args")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(String::from).collect())
                    .unwrap_or_default();
                let env: Vec<(String, String)> = call.arguments.get("env")
                    .and_then(|v| v.as_object())
                    .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect())
                    .unwrap_or_default();
                let transport = call.arguments.get("transport").and_then(|v| v.as_str()).map(String::from);
                let endpoint = call.arguments.get("endpoint").and_then(|v| v.as_str()).map(String::from);
                ManagementCommand::RegisterMcpServer { name, command, args, env, transport, endpoint }
            }
            "remove_mcp_server" => {
                let name = call.arguments.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                ManagementCommand::RemoveMcpServer { name }
            }
            "set_mcp_enabled" => {
                let name = call.arguments.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let enabled = call.arguments.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                ManagementCommand::SetMcpServerEnabled { name, enabled }
            }
            "install_skill" => {
                let path = call.arguments.get("path").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                if path.is_empty() {
                    return Some(ToolResult {
                        ok: false, summary: "path 为必填参数".to_string(),
                        stdout: String::new(), stderr: String::new(), exit_code: 1, execution: None,
                    });
                }
                let enabled = call.arguments.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                ManagementCommand::InstallSkill { path, enabled }
            }
            "remove_skill" => {
                let id = call.arguments.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                ManagementCommand::RemoveSkill { id }
            }
            "set_skill_enabled" => {
                let id = call.arguments.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let enabled = call.arguments.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                ManagementCommand::SetSkillEnabled { id, enabled }
            }
            _ => return None,
        };

        let desc = match &cmd {
            ManagementCommand::RegisterMcpServer { name, .. } => format!("注册 MCP 服务器：{name}"),
            ManagementCommand::RemoveMcpServer { name } => format!("移除 MCP 服务器：{name}"),
            ManagementCommand::SetMcpServerEnabled { name, enabled } => format!("{}MCP 服务器：{name}", if *enabled { "启用" } else { "禁用" }),
            ManagementCommand::InstallSkill { path, .. } => format!("安装 Skill：{path}"),
            ManagementCommand::RemoveSkill { id } => format!("卸载 Skill：{id}"),
            ManagementCommand::SetSkillEnabled { id, enabled } => format!("{}Skill：{id}", if *enabled { "启用" } else { "禁用" }),
        };

        on_cmd(cmd);

        Some(ToolResult {
            ok: true,
            summary: format!("{desc}，操作已提交"),
            stdout: format!("{desc}，将在当前执行完成后生效"),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }

    fn handle_get_skill_detail(&self, call: &ModelFunctionCall) -> ToolResult {
        let skill_id = call
            .arguments
            .get("skill_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let skill = self
            .agent_config
            .skills
            .installed
            .iter()
            .find(|s| s.id == skill_id && s.enabled);

        let Some(skill) = skill else {
            return ToolResult {
                ok: false,
                summary: format!("未找到 skill：{skill_id}"),
                stdout: String::new(),
                stderr: format!("可用的 skill：{}", self.agent_config.skills.installed.iter().filter(|s| s.enabled).map(|s| s.id.as_str()).collect::<Vec<_>>().join(", ")),
                exit_code: 1,
                execution: None,
            };
        };

        let skill_dir = &skill.source.value;
        let skill_md = std::path::Path::new(skill_dir).join(&skill.entry);
        match std::fs::read_to_string(&skill_md) {
            Ok(content) => {
                let resolved = content.replace("{skill_dir}", skill_dir);
                ToolResult {
                    ok: true,
                    summary: format!("Skill {} 的使用说明", skill.name),
                    stdout: resolved,
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                }
            }
            Err(e) => ToolResult {
                ok: false,
                summary: format!("读取 skill 说明失败：{e}"),
                stdout: String::new(),
                stderr: e.to_string(),
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
                let width = call.arguments.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let height = call.arguments.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let style = call.arguments.get("style").and_then(|v| v.as_str()).map(String::from);
                let request = tiangong_media::image::ImageGenRequest {
                    prompt,
                    negative_prompt: None,
                    width,
                    height,
                    model: Some(resolved.model.clone()),
                    style,
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
            // 视频生成暂通过 skill 机制处理
            ModelCapability::VideoGeneration => ToolResult {
                ok: false,
                summary: "视频生成请通过 skill 调用".to_string(),
                stdout: String::new(),
                stderr: "视频生成功能暂未内置，请安装对应的视频生成 skill".to_string(),
                exit_code: 1,
                execution: None,
            },
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

    /// 处理语音合成（TTS）工具调用
    fn handle_tts(&self, call: &ModelFunctionCall) -> ToolResult {
        let text = call.arguments.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if text.is_empty() {
            return ToolResult {
                ok: false, summary: "缺少 text 参数".to_string(),
                stdout: String::new(), stderr: "text 不能为空".to_string(),
                exit_code: 1, execution: None,
            };
        }

        let resolved = match self.models_config.resolve_for_capability(ModelCapability::Tts) {
            Some(r) => r,
            None => return ToolResult {
                ok: false, summary: "TTS 能力未配置".to_string(),
                stdout: String::new(), stderr: "请在设置中配置 TTS 模型路由".to_string(),
                exit_code: 1, execution: None,
            },
        };

        let voice = call.arguments.get("voice").and_then(|v| v.as_str()).map(String::from);
        let speed = call.arguments.get("speed").and_then(|v| v.as_f64());
        let output_path = call.arguments.get("output_path")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                let dir = std::path::PathBuf::from(home).join(".tiangong").join("media");
                let _ = std::fs::create_dir_all(&dir);
                dir.join(format!("tts_{}.mp3", scru128::new()))
                    .to_string_lossy().to_string()
            });

        let started = std::time::Instant::now();
        let tool_name = call.name.clone();

        use tiangong_media::tts::SpeechSynthesizer;
        let synthesizer = tiangong_media::openai_tts::OpenAITTS::new(
            resolved.api_key.clone(),
            resolved.base_url.clone(),
        );
        let request = tiangong_media::tts::SynthesizeRequest {
            text: text.clone(),
            voice,
            speed,
            model: Some(resolved.model.clone()),
            output_format: Some("mp3".to_string()),
        };

        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => return ToolResult {
                ok: false, summary: format!("运行时初始化失败：{e}"),
                stdout: String::new(), stderr: e.to_string(),
                exit_code: 1, execution: None,
            },
        };

        let result: Result<Result<tiangong_media::tts::SynthesizeResponse, anyhow::Error>, _> = runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                synthesizer.synthesize(request),
            ).await
        });

        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(Ok(resp)) => {
                // 写入文件
                match std::fs::write(&output_path, &resp.audio) {
                    Ok(_) => {
                        let duration_info = resp.duration
                            .map(|d| format!("，时长 {:.1}s", d))
                            .unwrap_or_default();
                        ToolResult {
                            ok: true,
                            summary: format!("语音合成成功（模型：{}{}）", resolved.model, duration_info),
                            stdout: format!("音频文件已保存到：{output_path}"),
                            stderr: String::new(),
                            exit_code: 0,
                            execution: Some(ToolExecutionRecord {
                                tool_name, args: vec![], duration_ms,
                                ok: true, exit_code: 0,
                                summary: format!("语音合成成功（模型：{}）", resolved.model),
                            }),
                        }
                    }
                    Err(e) => ToolResult {
                        ok: false, summary: format!("音频文件写入失败：{e}"),
                        stdout: String::new(), stderr: e.to_string(),
                        exit_code: 1, execution: None,
                    },
                }
            }
            Ok(Err(e)) => ToolResult {
                ok: false, summary: format!("语音合成失败：{e}"),
                stdout: String::new(), stderr: e.to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name, args: vec![], duration_ms,
                    ok: false, exit_code: 1,
                    summary: format!("语音合成失败：{e}"),
                }),
            },
            Err(_) => ToolResult {
                ok: false, summary: "语音合成超时（60秒）".to_string(),
                stdout: String::new(), stderr: "timeout".to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name, args: vec![], duration_ms,
                    ok: false, exit_code: 1,
                    summary: "语音合成超时".to_string(),
                }),
            },
        }
    }

    /// 处理语音识别（STT）工具调用
    fn handle_stt(&self, call: &ModelFunctionCall) -> ToolResult {
        let file_path = call.arguments.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if file_path.is_empty() {
            return ToolResult {
                ok: false, summary: "缺少 file_path 参数".to_string(),
                stdout: String::new(), stderr: "file_path 不能为空".to_string(),
                exit_code: 1, execution: None,
            };
        }

        let resolved = match self.models_config.resolve_for_capability(ModelCapability::Stt) {
            Some(r) => r,
            None => return ToolResult {
                ok: false, summary: "STT 能力未配置".to_string(),
                stdout: String::new(), stderr: "请在设置中配置 STT 模型路由".to_string(),
                exit_code: 1, execution: None,
            },
        };

        // 读取音频文件
        let audio_data = match std::fs::read(&file_path) {
            Ok(data) => data,
            Err(e) => return ToolResult {
                ok: false, summary: format!("读取音频文件失败：{e}"),
                stdout: String::new(), stderr: e.to_string(),
                exit_code: 1, execution: None,
            },
        };

        // 根据扩展名推断 MIME 类型
        let mime_type = match std::path::Path::new(&file_path).extension().and_then(|e| e.to_str()) {
            Some("mp3") => "audio/mpeg",
            Some("wav") => "audio/wav",
            Some("ogg") | Some("oga") => "audio/ogg",
            Some("flac") => "audio/flac",
            Some("webm") => "audio/webm",
            Some("m4a") => "audio/mp4",
            _ => "audio/mpeg",
        }.to_string();

        let language = call.arguments.get("language").and_then(|v| v.as_str()).map(String::from);

        let started = std::time::Instant::now();
        let tool_name = call.name.clone();

        use tiangong_media::stt::SpeechRecognizer;
        let recognizer = tiangong_media::openai_stt::OpenAIWhisper::new(
            resolved.api_key.clone(),
            resolved.base_url.clone(),
        );
        let request = tiangong_media::stt::TranscribeRequest {
            audio: audio_data,
            mime_type,
            language,
            model: Some(resolved.model.clone()),
        };

        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => return ToolResult {
                ok: false, summary: format!("运行时初始化失败：{e}"),
                stdout: String::new(), stderr: e.to_string(),
                exit_code: 1, execution: None,
            },
        };

        let result: Result<Result<tiangong_media::stt::TranscribeResponse, anyhow::Error>, _> = runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(120),
                recognizer.transcribe(request),
            ).await
        });

        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(Ok(resp)) => {
                let lang_info = resp.language.as_deref()
                    .map(|l| format!("，语言：{l}"))
                    .unwrap_or_default();
                let dur_info = resp.duration
                    .map(|d| format!("，音频时长：{:.1}s", d))
                    .unwrap_or_default();
                ToolResult {
                    ok: true,
                    summary: format!("语音识别成功（模型：{}{}{dur_info}）", resolved.model, lang_info),
                    stdout: resp.text,
                    stderr: String::new(),
                    exit_code: 0,
                    execution: Some(ToolExecutionRecord {
                        tool_name, args: vec![], duration_ms,
                        ok: true, exit_code: 0,
                        summary: format!("语音识别成功（模型：{}）", resolved.model),
                    }),
                }
            }
            Ok(Err(e)) => ToolResult {
                ok: false, summary: format!("语音识别失败：{e}"),
                stdout: String::new(), stderr: e.to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name, args: vec![], duration_ms,
                    ok: false, exit_code: 1,
                    summary: format!("语音识别失败：{e}"),
                }),
            },
            Err(_) => ToolResult {
                ok: false, summary: "语音识别超时（120秒）".to_string(),
                stdout: String::new(), stderr: "timeout".to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name, args: vec![], duration_ms,
                    ok: false, exit_code: 1,
                    summary: "语音识别超时".to_string(),
                }),
            },
        }
    }

    pub fn fallback_error_message(err: &anyhow::Error) -> String {
        format!("执行失败：{err}")
    }

    /// 判断用户输入是否为简单对话（不需要工具调用）
    /// 简单对话跳过工具注入，大幅减少 prompt_tokens
    fn is_simple_chat(input: &str) -> bool {
        let input = input.trim();
        // 空输入不走快速路径（避免误判）
        if input.is_empty() {
            return false;
        }
        // 包含明确工具触发关键词的不走快速路径
        let tool_keywords = [
            "文件", "目录", "代码", "搜索", "执行", "运行", "命令", "终端",
            "创建", "删除", "修改", "编辑", "写入", "读取", "查看",
            "图片", "生成图", "画", "视频", "语音", "播放", "录音",
            "安装", "卸载", "skill", "mcp", "@",
            "编译", "构建", "build", "deploy", "git",
            "下载", "上传", "curl", "wget",
        ];
        let lower = input.to_lowercase();
        for kw in &tool_keywords {
            if lower.contains(kw) {
                return false;
            }
        }
        // 短输入（< 100 字符）且不包含工具关键词 → 简单对话
        if input.len() < 100 {
            return true;
        }
        false
    }
}

/// 构建 ReAct agent 的系统 prompt
fn build_react_system_prompt(
    user_input: &str,
    models_config: &ModelsConfig,
    agent_config: &AgentConfig,
) -> String {
    use crate::models_config::ModelCapability;

    // 构建多媒体能力提示
    let mut media_hints = Vec::new();
    for cap in ModelCapability::media_capabilities() {
        if let Some(resolved) = models_config.resolve_for_capability(*cap) {
            media_hints.push(format!(
                "- {}：已配置（模型：{}）",
                cap.display_name(),
                resolved.model
            ));
        }
    }
    let media_section = if media_hints.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n已配置的多媒体能力：\n{}",
            media_hints.join("\n")
        )
    };

    // 构建已安装 skill 摘要（仅名称+描述，不注入完整 SKILL.md）
    let mut skill_summaries = Vec::new();
    for skill in &agent_config.skills.installed {
        if !skill.enabled {
            continue;
        }
        skill_summaries.push(format!(
            "- {} (id={}): {}",
            skill.name,
            skill.id,
            if skill.description.is_empty() { "无描述" } else { &skill.description }
        ));
    }
    let skills_section = if skill_summaries.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n已安装的 Skills（使用前先调用 get_skill_detail 获取完整说明）：\n{}",
            skill_summaries.join("\n")
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
7. 工具调用失败时必须如实告知用户失败原因，绝对不能虚构成功结果。
8. 如果已安装的 Skill 能处理用户请求，优先通过 run_command 调用 Skill 脚本。
9. 耗时较长的命令（编译、下载、视频生成等）使用 spawn_task 在后台执行。
10. 多个可并行的耗时任务使用 spawn+join 模式：先多次调用 spawn_task 启动所有任务，再调用 wait_tasks 一次性等待全部完成。
11. 独立的后台任务（不需要等待结果的）用 spawn_task 启动后直接继续，无需 wait_tasks。{media_section}{skills_section}

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
