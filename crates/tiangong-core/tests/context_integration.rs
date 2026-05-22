//! 上下文系统集成测试
//!
//! 验证 PromptAssembler、ContextCompressor、ContextOrganizer 在重构后的协作行为：
//! - 消息零丢失：所有 session.messages 完整传递给 LLM
//! - 消息顺序：capabilities → memory → team → summary → attachments → history
//! - 压缩流程：mock LLM 返回固定摘要，验证 summary_up_to 推进

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use tiangong_core::agent_config::AgentConfig;
use tiangong_core::context::compressor::ContextCompressor;
use tiangong_core::context::organizer::ContextOrganizer;
use tiangong_core::model::{ModelProviderConfig, SingleProviderClient};
use tiangong_core::prompt::PromptAssembler;
use tiangong_core::session::{Message, MessageRole, MessageToolCall, Session};
use tiangong_llm::ProviderProtocol;

// ── Mock LLM 服务器 ──────────────────────────────────────────

struct MockLlmServer {
    base_url: String,
    shutdown_tx: Option<mpsc::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl MockLlmServer {
    /// 启动一个 mock OpenAI 兼容服务器，所有请求返回固定摘要文本。
    fn start(response_text: &str) -> Self {
        let response_text = response_text.to_string();
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 mock LLM 端口失败");
        let addr = listener.local_addr().expect("读取 mock LLM 地址失败");
        listener
            .set_nonblocking(true)
            .expect("设置 mock LLM 非阻塞失败");
        let base_url = format!("http://{}", addr);
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let join = std::thread::spawn(move || {
            loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_http_request(&mut stream);
                        if let Some(_body) = request {
                            let resp = openai_chat_completion_response(&response_text);
                            let http = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                resp.len(),
                                resp
                            );
                            let _ = stream.write_all(http.as_bytes());
                            let _ = stream.flush();
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base_url,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for MockLlmServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.take().unwrap().send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8(buffer[header_end..header_end + content_length].to_vec()).ok()
}

fn openai_chat_completion_response(content: &str) -> String {
    let escaped = serde_json::to_string(content).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"{{"id":"chatcmpl-mock","object":"chat.completion","model":"mock-model","choices":[{{"index":0,"message":{{"role":"assistant","content":{escaped}}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}}}"#
    )
}

fn mock_client(base_url: &str) -> SingleProviderClient {
    SingleProviderClient::new(ModelProviderConfig {
        api_auth_token: "test-token".to_string(),
        api_base_url: base_url.to_string(),
        api_timeout_ms: "5000".to_string(),
        api_protocol: ProviderProtocol::OpenAiCompatible,
        api_model: "mock-model".to_string(),
        api_lite_model: String::new(),
    })
}

// ── 辅助工具 ─────────────────────────────────────────────────

fn helper_session() -> Session {
    Session::new("测试会话")
}

fn user_msg(content: &str) -> Message {
    Message::new(MessageRole::User, content)
}

fn tool_context_msg(name: &str, content: &str) -> Message {
    let mut msg = Message::new(MessageRole::Tool, content);
    msg.tool_name = Some(name.to_string());
    msg
}

fn tool_result_msg(tool_call_id: &str, content: &str) -> Message {
    let mut msg = Message::new(MessageRole::Tool, content);
    msg.tool_call_id = Some(tool_call_id.to_string());
    msg
}

/// 多轮对话（3 轮 user-assistant-tool_result）
fn multi_turn_session() -> Session {
    let mut session = helper_session();
    // 第 1 轮
    session.append_message(MessageRole::User, "请读取 main.rs");
    session.append_message(MessageRole::Assistant, "我来读取 main.rs");
    {
        let last = session.messages.last_mut().unwrap();
        last.tool_calls.push(MessageToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "main.rs"}),
        });
    }
    let mut tr1 = tool_result_msg("call_1", "fn main() { println!(\"hello\"); }");
    tr1.tool_name = Some("read_file".to_string());
    session.messages.push(tr1);
    // 第 2 轮
    session.append_message(MessageRole::User, "把 hello 改成 hi");
    session.append_message(MessageRole::Assistant, "已修改 main.rs");
    {
        let last = session.messages.last_mut().unwrap();
        last.tool_calls.push(MessageToolCall {
            id: "call_2".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({"path": "main.rs", "content": "fn main() { println!(\"hi\"); }"}),
        });
    }
    let mut tr2 = tool_result_msg("call_2", "写入成功");
    tr2.tool_name = Some("write_file".to_string());
    session.messages.push(tr2);
    // 第 3 轮
    session.append_message(MessageRole::User, "运行一下");
    session.append_message(MessageRole::Assistant, "运行结果：hi");
    session
}

// ── PromptAssembler 测试 ─────────────────────────────────────

#[test]
fn assemble_empty_session_produces_minimal_context() {
    let session = helper_session();
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "你好",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    // 无 context_summary、无 attachments、无 memory、无 team
    assert!(result.context_summary_message.is_none());
    assert!(result.memory_prefix_message.is_none());
    assert!(result.team_context_message.is_none());
    assert!(result.attachment_messages.is_empty());
    // history 只有空 session（无消息）
    assert!(result.history_messages.is_empty());
    assert_eq!(result.user_input, "你好");
}

#[test]
fn assemble_preserves_all_session_messages_in_history() {
    let session = multi_turn_session();
    let msg_count = session.messages.len();
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    // 所有 session.messages 必须出现在 history 中
    assert_eq!(result.history_messages.len(), msg_count);
    for (original, assembled) in session.messages.iter().zip(result.history_messages.iter()) {
        assert_eq!(original.content, assembled.content);
        assert_eq!(original.role, assembled.role);
    }
}

#[test]
fn memory_context_injected_as_prefix_tool_message() {
    let session = helper_session();
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        Some("用户之前问过关于 Rust 的问题"),
        None,
    );
    let mem = result
        .memory_prefix_message
        .expect("应有 memory_prefix_message");
    assert_eq!(mem.role, MessageRole::Tool);
    assert!(mem.content.contains("用户之前问过关于 Rust 的问题"));
    assert!(mem.content.contains("<memory-recall>"));
    assert_eq!(mem.tool_name.as_deref(), Some("recall_memory"));
}

#[test]
fn team_context_injected_as_prefix_message() {
    let session = helper_session();
    let team_msg = Message::new(MessageRole::System, "你是 sub-agent，团队有 3 个成员");
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        Some(&team_msg),
    );
    let team = result
        .team_context_message
        .expect("应有 team_context_message");
    assert_eq!(team.role, MessageRole::System);
    assert!(team.content.contains("sub-agent"));
}

#[test]
fn context_summary_injected_when_present() {
    let mut session = helper_session();
    session.append_message(MessageRole::User, "你好");
    session.append_message(MessageRole::Assistant, "你好！");
    session.append_message(MessageRole::User, "继续");
    session.append_message(MessageRole::Assistant, "好的");
    session.context_summary = Some("之前的对话讨论了 Rust 基础知识".to_string());
    session.summary_up_to = 2; // 前两条消息已被摘要
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    let summary = result
        .context_summary_message
        .expect("应有 context_summary_message");
    assert_eq!(summary.role, MessageRole::Tool);
    assert!(summary.content.contains("之前的对话讨论了 Rust 基础知识"));
    assert!(summary.content.contains("<context-summary>"));
    // history 应只有后两条消息
    assert_eq!(result.history_messages.len(), 2);
}

#[test]
fn only_messages_after_summary_up_to_included() {
    let mut session = multi_turn_session();
    let total = session.messages.len();
    // 模拟已压缩前 4 条消息
    session.summary_up_to = 4;
    session.context_summary = Some("早期对话摘要".to_string());
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    // history 应只包含 summary_up_to 之后的消息
    assert_eq!(result.history_messages.len(), total - 4);
    assert!(result.context_summary_message.is_some());
}

#[test]
fn full_message_order_is_correct() {
    let mut session = helper_session();
    session.append_message(MessageRole::User, "你好");
    session.context_summary = Some("摘要内容".to_string());
    session.summary_up_to = 0;
    let team_msg = Message::new(MessageRole::System, "团队信息");
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        Some("记忆内容"),
        Some(&team_msg),
    );
    let messages = result.build_messages();
    // 验证顺序：
    // 1. capabilities（Tool，有 system_prompt 内容）
    // 2. memory（Tool，<memory-recall>）
    // 3. team（System）
    // 4. context_summary（Tool，<context-summary>）
    // 5. attachments（MCP tools，可能为空）
    // 6. history（User "你好"）
    let mut idx = 0;
    // capabilities
    if !result.system_prompt.is_empty() {
        assert_eq!(messages[idx].role, MessageRole::Tool);
        assert!(messages[idx].content.contains("<capabilities>"));
        idx += 1;
    }
    // memory
    assert_eq!(messages[idx].role, MessageRole::Tool);
    assert!(messages[idx].content.contains("<memory-recall>"));
    idx += 1;
    // team
    assert_eq!(messages[idx].role, MessageRole::System);
    assert!(messages[idx].content.contains("团队信息"));
    idx += 1;
    // context_summary
    assert_eq!(messages[idx].role, MessageRole::Tool);
    assert!(messages[idx].content.contains("<context-summary>"));
    idx += 1;
    // attachments 可能为空或为 MCP tools
    let attachment_count = result.attachment_messages.len();
    idx += attachment_count;
    // history
    assert!(idx < messages.len());
    assert_eq!(messages[idx].role, MessageRole::User);
    assert_eq!(messages[idx].content, "你好");
}

#[test]
fn multi_turn_preserves_complete_history() {
    let session = multi_turn_session();
    let total = session.messages.len();
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    let _messages = result.build_messages();
    // history 部分（去掉可能的 prefix 消息）应包含所有 session.messages
    let history = &result.history_messages;
    assert_eq!(history.len(), total);
    // 验证每条消息角色和内容
    assert_eq!(history[0].role, MessageRole::User);
    assert_eq!(history[1].role, MessageRole::Assistant);
    assert_eq!(history[2].role, MessageRole::Tool);
    assert_eq!(history[3].role, MessageRole::User);
    assert_eq!(history[4].role, MessageRole::Assistant);
    assert_eq!(history[5].role, MessageRole::Tool);
    assert_eq!(history[6].role, MessageRole::User);
    assert_eq!(history[7].role, MessageRole::Assistant);
}

#[test]
fn tool_context_messages_preserved_in_history() {
    let mut session = helper_session();
    // 模拟完成度检查提示（直接推入 session.messages）
    session.append_message(MessageRole::User, "请帮我写代码");
    session.append_message(MessageRole::Assistant, "好的，让我来写");
    session.messages.push(tool_context_msg(
        "react_completion_check",
        "上方回复被判定为未完成任务",
    ));
    session.append_message(MessageRole::Assistant, "代码已完成");
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    // 所有 4 条消息都应出现在 history 中
    assert_eq!(result.history_messages.len(), 4);
    let tool_ctx = &result.history_messages[2];
    assert_eq!(tool_ctx.role, MessageRole::Tool);
    assert_eq!(
        tool_ctx.tool_name.as_deref(),
        Some("react_completion_check")
    );
}

#[test]
fn failure_recovery_reminder_preserved_in_history() {
    let mut session = helper_session();
    session.append_message(MessageRole::User, "读取文件");
    session.append_message(MessageRole::Assistant, "调用 read_file");
    // 模拟工具执行失败
    let mut tr = tool_result_msg("call_1", "文件不存在");
    tr.tool_name = Some("read_file".to_string());
    tr.tool_result_is_error = true;
    session.messages.push(tr);
    // 失败恢复提示（直接推入 session.messages）
    session.messages.push(tool_context_msg(
        "react_failed_tool_recovery",
        "read_file 调用失败",
    ));
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    assert_eq!(result.history_messages.len(), 4);
    assert_eq!(result.history_messages[3].role, MessageRole::Tool);
    assert_eq!(
        result.history_messages[3].tool_name.as_deref(),
        Some("react_failed_tool_recovery")
    );
}

#[test]
fn sub_agent_messages_preserved_in_history() {
    let mut session = helper_session();
    session.append_message(MessageRole::User, "请分析代码");
    // 模拟子代理消息（直接推入 session.messages）
    let sub_msg = user_msg("[from:agent-1 at 2025-01-01T00:00:00]\n分析完成");
    session.messages.push(sub_msg);
    let assembler = PromptAssembler::new(32768);
    let result = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    assert_eq!(result.history_messages.len(), 2);
    assert!(result.history_messages[1].content.contains("[from:agent-1"));
}

// ── ContextCompressor 测试（不需要 LLM）────────────────────

#[test]
fn compressor_build_context_keeps_recent_turns() {
    let compressor = ContextCompressor::new(2); // 保留最近 2 轮
    let mut session = helper_session();
    // 5 轮对话
    for i in 0..5 {
        session.append_message(MessageRole::User, format!("用户消息 {i}"));
        session.append_message(MessageRole::Assistant, format!("助手回复 {i}"));
    }
    // 无 summary 时，build_context 返回全部消息
    let context = compressor.build_context(&session);
    assert_eq!(context.len(), session.messages.len());
}

#[test]
fn compressor_no_split_for_few_messages() {
    let compressor = ContextCompressor::new(6);
    let mut session = helper_session();
    session.append_message(MessageRole::User, "只有一条");
    let context = compressor.build_context(&session);
    // 消息太少，全部返回
    assert_eq!(context.len(), 1);
}

#[test]
fn build_context_returns_all_messages_when_no_summary() {
    let session = multi_turn_session();
    let total = session.messages.len();
    let compressor = ContextCompressor::new(6);
    let context = compressor.build_context(&session);
    assert_eq!(context.len(), total);
}

#[test]
fn build_context_returns_only_recent_after_summary() {
    let mut session = multi_turn_session();
    let total = session.messages.len();
    session.summary_up_to = 4;
    let compressor = ContextCompressor::new(6);
    let context = compressor.build_context(&session);
    assert_eq!(context.len(), total - 4);
}

// ── ContextOrganizer 测试 ────────────────────────────────────

#[test]
fn organizer_needs_compression_below_threshold() {
    let organizer = ContextOrganizer::new(10000).with_threshold(0.95);
    assert!(!organizer.needs_compression(5000));
    assert!(!organizer.needs_compression(9499));
    assert!(!organizer.needs_compression(9500)); // == threshold 不触发
    assert!(organizer.needs_compression(9501)); // > threshold 才触发
    assert!(organizer.needs_compression(10000));
}

#[test]
fn organizer_build_context_matches_compressor() {
    let mut session = multi_turn_session();
    session.summary_up_to = 3;
    let organizer = ContextOrganizer::new(32768);
    let context = organizer.build_context(&session);
    assert_eq!(context.len(), session.messages.len() - 3);
}

// ── Mock LLM 压缩流程测试 ────────────────────────────────────

#[test]
fn compression_with_mock_llm_produces_summary() {
    let mock = MockLlmServer::start("这是一个关于多轮对话的压缩摘要");
    let client = mock_client(mock.base_url());
    let mut session = multi_turn_session();
    let original_len = session.messages.len();
    let compressor = ContextCompressor::new(2); // 保留最近 2 轮
    let result = compressor
        .update_summary_with_usage(&mut session, &client)
        .expect("压缩不应失败");
    assert!(result.compressed);
    assert!(session.context_summary.is_some());
    assert!(
        session
            .context_summary
            .as_ref()
            .unwrap()
            .contains("压缩摘要")
    );
    assert!(session.summary_up_to > 0);
    assert!(session.summary_up_to < original_len);
}

#[test]
fn after_compression_assembly_excludes_summarized_messages() {
    let mock = MockLlmServer::start("早期对话讨论了文件读取和修改");
    let client = mock_client(mock.base_url());
    let mut session = multi_turn_session();
    let total = session.messages.len();
    // 执行压缩
    let compressor = ContextCompressor::new(2);
    let result = compressor
        .update_summary_with_usage(&mut session, &client)
        .expect("压缩不应失败");
    assert!(result.compressed);
    let summary_up_to = session.summary_up_to;
    assert!(summary_up_to > 0);
    // 装配后 history 应只有 summary_up_to 之后的消息
    let assembler = PromptAssembler::new(32768);
    let assembled = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    assert!(assembled.context_summary_message.is_some());
    assert_eq!(assembled.history_messages.len(), total - summary_up_to);
    // 验证 context_summary 消息内容
    let summary_msg = assembled.context_summary_message.unwrap();
    assert!(summary_msg.content.contains("早期对话讨论了文件读取和修改"));
    assert!(summary_msg.content.contains("<context-summary>"));
}

#[test]
fn double_compression_only_advances_if_new_messages() {
    let mock = MockLlmServer::start("第一次压缩摘要");
    let client = mock_client(mock.base_url());
    let mut session = multi_turn_session();
    let compressor = ContextCompressor::new(2);
    // 第一次压缩
    let result1 = compressor
        .update_summary_with_usage(&mut session, &client)
        .expect("第一次压缩不应失败");
    assert!(result1.compressed);
    let first_summary_up_to = session.summary_up_to;
    // 不添加新消息，第二次压缩应无效（split_point <= summary_up_to）
    let result2 = compressor
        .update_summary_with_usage(&mut session, &client)
        .expect("第二次压缩不应失败");
    assert!(!result2.compressed);
    assert_eq!(session.summary_up_to, first_summary_up_to);
}

// ── Provider 消息转换测试 ────────────────────────────────────

#[test]
fn tool_context_messages_appear_in_provider_messages() {
    let mut session = helper_session();
    session.append_message(MessageRole::User, "你好");
    session
        .messages
        .push(tool_context_msg("capabilities", "Skills: 无"));
    session
        .messages
        .push(tool_context_msg("recall_memory", "回忆结果内容"));
    let assembler = PromptAssembler::new(32768);
    let assembled = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    let messages = assembled.build_messages();
    // 至少应有 User + 2 个 Tool context 消息
    assert!(messages.len() >= 3);
    // Tool context 消息（无 tool_call_id）应在 messages 中
    let tool_contexts: Vec<_> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool && m.tool_call_id.is_none())
        .collect();
    assert!(tool_contexts.len() >= 2);
}

#[test]
fn force_final_response_reminder_included_in_assembly() {
    let mut session = multi_turn_session();
    // 模拟 force_final_response 推入的提醒
    session.messages.push(tool_context_msg(
        "force_final_response",
        "请基于以上所有工具执行结果，直接给出最终回复。",
    ));
    let assembler = PromptAssembler::new(32768);
    let assembled = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    // 提醒消息应在 history 末尾
    let last = assembled.history_messages.last().expect("应有消息");
    assert_eq!(last.role, MessageRole::Tool);
    assert_eq!(last.tool_name.as_deref(), Some("force_final_response"));
}

#[test]
fn no_duplicate_messages_in_assembly() {
    let session = multi_turn_session();
    let assembler = PromptAssembler::new(32768);
    let assembled = assembler.assemble(
        &session,
        "",
        Vec::new(),
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        None,
        None,
    );
    // 每条 history 消息的 id 应唯一
    let mut ids = std::collections::HashSet::new();
    for msg in &assembled.history_messages {
        assert!(ids.insert(msg.id.clone()), "发现重复消息 id: {}", msg.id);
    }
}
