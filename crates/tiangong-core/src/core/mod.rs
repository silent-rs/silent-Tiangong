//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::agent_config::AgentConfig;
use crate::agents::execution_mcp_agent::{McpFunctionTarget, execution_function_tools};
use crate::app_state::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::model::{
    FunctionToolSpec, ModelClient, ModelProviderConfig, ModelRequest, ModelStreamChunk,
    SingleProviderClient, TokenUsage,
};
use crate::models_config::ModelsConfig;
use crate::prompt::PromptAssembler;
use crate::runtime::{LlmOutputRecord, RuntimeEngine, inject_enhanced_tools, use_stream_mode};
use crate::session::{Message, MessageRole, Session, now_text};
use tiangong_types::StreamEvent;

const DEFAULT_CONTEXT_LIMIT: usize = 32_768;
const MAX_ROUNDS: usize = 20;

/// 用户命令
enum Command {
    /// 发送消息
    Message(String),
    /// 取消当前执行
    Cancel,
    /// 审批响应
    #[allow(dead_code)]
    Approval { request_id: String, approved: bool },
    /// 关闭
    Shutdown,
}

/// 天工智能体核心
pub struct TiangongCore {
    /// 用户命令发送端
    cmd_tx: Option<Sender<Command>>,
    /// 工作线程
    worker: Option<JoinHandle<Session>>,
    /// 会话 ID
    session_id: String,
}

impl TiangongCore {
    /// 创建新对话
    pub fn new(stream_tx: Sender<StreamEvent>) -> Self {
        let session = Session::new("新对话");
        Self::with_session(session, stream_tx)
    }

    /// 从已有 session 创建
    pub fn with_session(session: Session, stream_tx: Sender<StreamEvent>) -> Self {
        let session_id = session.id.clone();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();

        let worker = thread::spawn(move || worker_loop(session, stream_tx, cmd_rx));

        Self {
            cmd_tx: Some(cmd_tx),
            worker: Some(worker),
            session_id,
        }
    }

    fn send_cmd(&self, cmd: Command) {
        if let Some(ref tx) = self.cmd_tx {
            let _ = tx.send(cmd);
        }
    }

    pub fn send_message(&self, content: String) {
        self.send_cmd(Command::Message(content));
    }

    pub fn cancel(&self) {
        self.send_cmd(Command::Cancel);
    }

    pub fn respond_approval(&self, request_id: String, approved: bool) {
        self.send_cmd(Command::Approval {
            request_id,
            approved,
        });
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 关闭并获取最终 session
    pub fn into_session(mut self) -> Session {
        self.send_cmd(Command::Shutdown);
        self.cmd_tx = None;
        if let Some(w) = self.worker.take() {
            match w.join() {
                Ok(session) => return session,
                Err(_) => tracing::warn!("TiangongCore worker panic"),
            }
        }
        Session::new("recovered")
    }
}

impl Drop for TiangongCore {
    fn drop(&mut self) {
        if let Some(ref tx) = self.cmd_tx {
            let _ = tx.send(Command::Shutdown);
        }
        self.cmd_tx = None;
    }
}

// ==================== 工作线程 ====================

/// 工作线程：接收用户命令，执行 LLM + 工具，推送 StreamEvent
fn worker_loop(
    mut session: Session,
    stream_tx: Sender<StreamEvent>,
    cmd_rx: Receiver<Command>,
) -> Session {
    let engine = build_engine();

    // 初始化工具
    let (all_tools, mcp_targets) = execution_function_tools(&engine.agent_config().mcp);
    let mut tools: Vec<FunctionToolSpec> = all_tools
        .into_iter()
        .filter(|t| t.name != "mark_step_completed")
        .collect();
    inject_enhanced_tools(&mut tools, engine.models_config(), engine.agent_config());

    // 设置工作目录
    if !session.cwd.is_empty() {
        let p = std::path::PathBuf::from(&session.cwd);
        if p.is_dir() {
            crate::tool::set_session_cwd(Some(p));
        }
    }

    while let Ok(cmd) = cmd_rx.recv() {

        match cmd {
            Command::Message(content) => {
                // 记录用户消息
                session.append_message(MessageRole::User, content.clone());

                // 执行对话轮次
                execute_turn(
                    &mut session,
                    &content,
                    &engine,
                    &tools,
                    &mcp_targets,
                    &stream_tx,
                    &cmd_rx,
                );
            }
            Command::Cancel => {
                // 当前简单处理：发送错误事件
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
            }
            Command::Approval { .. } => {
                // TODO: 审批响应处理
            }
            Command::Shutdown => break,
        }
    }

    session
}

/// 执行一个完整的对话轮次（可能多轮工具调用）
///
/// 每轮之间检查 cmd_rx：新消息注入上下文，cancel 立即生效。
fn execute_turn(
    session: &mut Session,
    user_input: &str,
    engine: &RuntimeEngine,
    tools: &[FunctionToolSpec],
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    stream_tx: &Sender<StreamEvent>,
    cmd_rx: &Receiver<Command>,
) {
    let mut loop_context: Vec<Message> = Vec::new();
    let mut round = 0;
    let mut accumulated_usage = TokenUsage::default();

    loop {
        if round >= MAX_ROUNDS {
            // 超限：强制最终回复
            force_final_response(session, &loop_context, engine, stream_tx);
            break;
        }

        // 构建 prompt
        let assembler = PromptAssembler::new(engine.context_limit);
        let llm_user_input = if round == 0 {
            user_input.to_string()
        } else {
            "根据上面的工具执行结果继续处理。如果已经收集到足够信息，直接给出最终回复，不要再调用工具。".to_string()
        };

        let assembled = assembler.assemble(
            session,
            &llm_user_input,
            tools.to_vec(),
            engine.models_config(),
            engine.agent_config(),
            &loop_context,
        );

        let system_prompt = assembled.final_system_prompt();
        let req = ModelRequest {
            session_title: session.title.clone(),
            user_input: assembled.user_input.clone(),
            context: assembled.build_messages(),
            assembled_system_prompt: Some(system_prompt),
        };

        // LLM 流式调用
        let tx = stream_tx.clone();
        let response = match engine.client().complete_with_functions_stream(
            &req,
            tools,
            &mut |delta: &ModelStreamChunk| {
                // 直接推送 StreamEvent
                if !delta.content.is_empty() {
                    let _ = tx.send(StreamEvent::Delta {
                        content: delta.content.clone(),
                    });
                }
                if !delta.reasoning_content.is_empty() {
                    let _ = tx.send(StreamEvent::Reasoning {
                        content: delta.reasoning_content.clone(),
                    });
                }
            },
        ) {
            Ok(r) => r,
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                return;
            }
        };

        accumulated_usage.accumulate(&response.usage);
        round += 1;

        if response.tool_calls.is_empty() {
            // 最终回复：记录到 session
            session.append_message_with_reasoning(
                MessageRole::Assistant,
                response.text.clone(),
                response.reasoning_content.clone(),
            );
            // 记录 LLM 输出
            let output = LlmOutputRecord {
                stage: format!("react-round-{round}"),
                content: String::new(),
                reasoning_content: String::new(),
                tool_calls: Vec::new(),
                usage: response.usage.clone(),
            };
            session.append_message(MessageRole::System, format_llm_output_message(&output));

            let _ = stream_tx.send(StreamEvent::Done {
                usage: Some(tiangong_types::TokenUsage {
                    prompt_tokens: accumulated_usage.prompt_tokens,
                    completion_tokens: accumulated_usage.completion_tokens,
                    total_tokens: accumulated_usage.total_tokens,
                }),
            });
            return;
        }

        // 工具调用
        let tool_names: Vec<String> =
            response.tool_calls.iter().map(|c| c.name.clone()).collect();

        // 记录 LLM 输出到 session
        let output = LlmOutputRecord {
            stage: format!("react-round-{round}"),
            content: response.text.clone(),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: tool_names.clone(),
            usage: response.usage.clone(),
        };
        session.append_message_with_reasoning(
            MessageRole::System,
            format_llm_output_message(&output),
            response.reasoning_content.clone(),
        );

        // 推送工具调用事件
        let _ = stream_tx.send(StreamEvent::ToolCalls {
            names: tool_names.clone(),
            usage: Some(tiangong_types::TokenUsage {
                prompt_tokens: response.usage.prompt_tokens,
                completion_tokens: response.usage.completion_tokens,
                total_tokens: response.usage.total_tokens,
            }),
        });

        // 记录 assistant 意图到 loop_context
        let assistant_text = if response.text.is_empty() {
            format!("[调用工具: {}]", tool_names.join(", "))
        } else {
            response.text.clone()
        };
        loop_context.push(Message {
            id: scru128::new().to_string(),
            role: MessageRole::Assistant,
            content: assistant_text,
            reasoning_content: response.reasoning_content.clone(),
            worker_id: None,
            created_at: now_text(),
        });

        // 执行工具
        for call in &response.tool_calls {
            let _ = stream_tx.send(StreamEvent::ToolStart {
                name: call.name.clone(),
                summary: String::new(),
            });

            let result =
                engine.execute_tool_call(call, mcp_targets, &engine.agent_config().mcp);

            let _ = stream_tx.send(StreamEvent::ToolResult {
                name: result.summary.clone(),
                ok: result.ok,
                output: result.stdout.clone(),
            });

            // 记录到 session
            session.append_message(MessageRole::System, format_tool_trace_message(&result));

            // 记录到 loop_context
            let feedback = format!(
                "工具 {} 执行{}：{}",
                call.name,
                if result.ok { "成功" } else { "失败" },
                if result.stdout.chars().count() > 2000 {
                    let truncated: String = result.stdout.chars().take(2000).collect();
                    format!("{truncated}...(截断)")
                } else if result.stdout.is_empty() {
                    result.summary.clone()
                } else {
                    result.stdout.clone()
                }
            );
            loop_context.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: feedback,
                reasoning_content: String::new(),
                worker_id: None,
                created_at: now_text(),
            });
        }

        // 每轮之间检查用户命令（非阻塞）
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Cancel => {
                    let _ = stream_tx.send(StreamEvent::Error {
                        message: "已取消".into(),
                    });
                    return;
                }
                Command::Message(content) => {
                    // 用户追加消息：注入 loop_context，下一轮 LLM 会看到
                    session.append_message(MessageRole::User, content.clone());
                    loop_context.push(Message {
                        id: scru128::new().to_string(),
                        role: MessageRole::User,
                        content,
                        reasoning_content: String::new(),
                        worker_id: None,
                        created_at: now_text(),
                    });
                }
                Command::Shutdown => return,
                _ => {}
            }
        }

        // 继续下一轮
    }
}

/// 超限时强制最终回复
fn force_final_response(
    session: &mut Session,
    loop_context: &[Message],
    engine: &RuntimeEngine,
    stream_tx: &Sender<StreamEvent>,
) {
    let assembler = PromptAssembler::new(engine.context_limit);
    let assembled = assembler.assemble(
        session,
        "请基于以上所有工具执行结果，直接给出最终回复。",
        Vec::new(),
        engine.models_config(),
        engine.agent_config(),
        loop_context,
    );

    let system_prompt = assembled.final_system_prompt();
    let req = ModelRequest {
        session_title: session.title.clone(),
        user_input: assembled.user_input.clone(),
        context: assembled.build_messages(),
        assembled_system_prompt: Some(system_prompt),
    };

    let tx = stream_tx.clone();
    let resp = if use_stream_mode() {
        match engine
            .client()
            .complete_stream_with_callback(&req, |delta| {
                if !delta.content.is_empty() {
                    let _ = tx.send(StreamEvent::Delta {
                        content: delta.content.clone(),
                    });
                }
            }) {
            Ok(r) => r,
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                return;
            }
        }
    } else {
        match engine.client().complete(&req) {
            Ok(r) => {
                if !r.text.is_empty() {
                    let _ = stream_tx.send(StreamEvent::Delta {
                        content: r.text.clone(),
                    });
                }
                r
            }
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                return;
            }
        }
    };

    session.append_message(MessageRole::Assistant, resp.text);
    let _ = stream_tx.send(StreamEvent::Done {
        usage: Some(tiangong_types::TokenUsage {
            prompt_tokens: resp.usage.prompt_tokens,
            completion_tokens: resp.usage.completion_tokens,
            total_tokens: resp.usage.total_tokens,
        }),
    });
}

fn build_engine() -> RuntimeEngine {
    let mut models_config = ModelsConfig::load();
    if models_config.is_empty() {
        let env_config = ModelProviderConfig::from_env();
        if !env_config.api_auth_token.is_empty() {
            models_config = ModelsConfig::from_legacy(&env_config);
        }
    }
    let model_config = models_config.to_chat_provider_config();
    let agent_config = AgentConfig::default();

    RuntimeEngine::new(
        SingleProviderClient::new(model_config),
        DEFAULT_CONTEXT_LIMIT,
        agent_config,
    )
    .with_models_config(models_config)
}
