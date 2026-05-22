//! 上下文系统集成测试
//!
//! 验证 session.context() + build_full_system_prompt 路径：
//! - system prompt 包含摘要和所有动态段
//! - 消息零丢失：所有 session.messages 完整传递给 LLM
//! - 压缩流程：mock LLM 返回固定摘要，验证 summary_up_to 推进

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use tiangong_core::agent_config::AgentConfig;
use tiangong_core::context::compressor::ContextCompressor;
use tiangong_core::model::{ModelClient, ModelProviderConfig, ModelRequest, SingleProviderClient};
use tiangong_core::prompt::SystemPromptConfig;
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

// ── 新路径（session.context + build_full_system_prompt）测试 ────────

/// 能捕获请求体的 Mock LLM 服务器
struct CapturingMockLlmServer {
    base_url: String,
    shutdown_tx: Option<mpsc::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
    /// 收到的所有请求体
    captured: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl CapturingMockLlmServer {
    fn start(response_text: &str) -> Self {
        let response_text = response_text.to_string();
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定端口失败");
        let addr = listener.local_addr().expect("读取地址失败");
        listener.set_nonblocking(true).expect("设置非阻塞失败");
        let base_url = format!("http://{}", addr);
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let captured: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let join = std::thread::spawn(move || {
            loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Some(body) = read_http_request(&mut stream) {
                            if let Ok(mut guard) = captured_clone.lock() {
                                guard.push(body);
                            }
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
            captured,
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 获取收到的请求体列表
    fn captured_requests(&self) -> Vec<String> {
        self.captured.lock().unwrap().clone()
    }
}

impl Drop for CapturingMockLlmServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.take().unwrap().send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn rebuild_session(session: &mut Session) {
    let config = SystemPromptConfig::from_configs(
        &tiangong_core::models_config::ModelsConfig::default(),
        &AgentConfig::default(),
        &session.id,
    );
    session.rebuild_system_prompt(&config);
}

#[test]
fn new_path_system_prompt_includes_summary() {
    let mut session = multi_turn_session();
    let total = session.messages.len();
    // 模拟压缩：设置 summary 并标记前 4 条消息已被摘要覆盖
    session.context_summary = Some("早期对话讨论了文件读取和文件修改操作".to_string());
    session.summary_up_to = 4;
    // 重建 system prompt
    rebuild_session(&mut session);
    // context() 应返回 [system_prompt_message] + messages[summary_up_to..]
    let context = session.context();
    // 第一条应为 System 消息
    assert_eq!(context[0].role, MessageRole::System);
    let system_text = &context[0].content;
    // 验证摘要已嵌入 system prompt
    assert!(
        system_text.contains("早期对话讨论了文件读取和文件修改操作"),
        "system prompt 应包含摘要内容，实际: {system_text}"
    );
    assert!(
        system_text.contains("此前对话摘要"),
        "system prompt 应包含摘要标签"
    );
    // 后续消息应只有 summary_up_to 之后的
    assert_eq!(context.len(), 1 + (total - 4));
    for msg in &context[1..] {
        assert_ne!(msg.role, MessageRole::System);
    }
}

#[test]
fn new_path_system_prompt_includes_all_sections() {
    let session = helper_session();
    let config = SystemPromptConfig {
        custom_prompt: "回复必须使用中文".to_string(),
        skills_text: "已安装的 Skills：\n- test-skill (id=s1): 测试技能".to_string(),
        media_text: "已配置的多媒体能力：\n- 图片生成：已配置".to_string(),
        team_text: "团队协作能力".to_string(),
        user_context: vec!["用户偏好深色主题".to_string()],
    };
    let msg = tiangong_core::prompt::sections::build_full_system_prompt(&session, &config);
    assert_eq!(msg.role, MessageRole::System);
    let text = &msg.content;
    // 静态段
    assert!(text.contains("天工智能助手"), "应包含身份段");
    assert!(text.contains("规则"), "应包含规则段");
    // 自定义指令
    assert!(text.contains("回复必须使用中文"), "应包含自定义指令");
    // 环境段
    assert!(text.contains("当前会话"), "应包含会话标题");
    assert!(text.contains("当前工作目录"), "应包含工作目录");
    // 动态段
    assert!(text.contains("test-skill"), "应包含 Skills 列表");
    assert!(text.contains("图片生成"), "应包含多媒体能力");
    assert!(text.contains("团队协作"), "应包含团队协作");
    // 用户上下文
    assert!(text.contains("用户偏好深色主题"), "应包含用户上下文");
}

#[test]
fn new_path_messages_increment_across_turns() {
    let mut session = helper_session();
    rebuild_session(&mut session);
    // 初始：system_prompt_message + 0 条消息
    let ctx0 = session.context();
    assert_eq!(ctx0.len(), 1); // 只有 system prompt message
    // 第 1 轮
    session.append_message(MessageRole::User, "你好");
    let ctx1 = session.context();
    assert_eq!(ctx1.len(), 2); // system + 1 条消息
    assert_eq!(ctx1[1].role, MessageRole::User);
    assert_eq!(ctx1[1].content, "你好");
    // 第 2 轮
    session.append_message(MessageRole::Assistant, "你好！有什么可以帮你的？");
    let ctx2 = session.context();
    assert_eq!(ctx2.len(), 3); // system + 2 条消息
    assert_eq!(ctx2[2].role, MessageRole::Assistant);
    // 第 3 轮
    session.append_message(MessageRole::User, "读取文件");
    session.append_message(MessageRole::Assistant, "正在读取");
    {
        let last = session.messages.last_mut().unwrap();
        last.tool_calls.push(MessageToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "test.rs"}),
        });
    }
    let mut tr = tool_result_msg("call_1", "文件内容");
    tr.tool_name = Some("read_file".to_string());
    session.messages.push(tr);
    let ctx3 = session.context();
    assert_eq!(ctx3.len(), 6); // system + 5 条消息
}

#[test]
fn new_path_tool_calls_preserved_in_context() {
    let mut session = helper_session();
    // 模拟一轮带工具调用的对话
    session.append_message(MessageRole::User, "请读取 main.rs");
    session.append_message(MessageRole::Assistant, "我来读取");
    {
        let last = session.messages.last_mut().unwrap();
        last.tool_calls.push(MessageToolCall {
            id: "call_abc".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "main.rs"}),
        });
    }
    let mut tr = tool_result_msg("call_abc", "fn main() {}");
    tr.tool_name = Some("read_file".to_string());
    session.messages.push(tr);
    session.append_message(MessageRole::Assistant, "文件内容如上");
    rebuild_session(&mut session);
    let context = session.context();
    // system + 4 条对话消息
    assert_eq!(context.len(), 5);
    // 验证 assistant 消息中的 tool_calls
    let assistant_msg = &context[2]; // system, user, assistant
    assert_eq!(assistant_msg.role, MessageRole::Assistant);
    assert_eq!(assistant_msg.tool_calls.len(), 1);
    assert_eq!(assistant_msg.tool_calls[0].id, "call_abc");
    assert_eq!(assistant_msg.tool_calls[0].name, "read_file");
    // 验证 tool result
    let tool_result = &context[3];
    assert_eq!(tool_result.role, MessageRole::Tool);
    assert_eq!(tool_result.tool_call_id.as_deref(), Some("call_abc"));
    assert_eq!(tool_result.content, "fn main() {}");
}

#[test]
fn new_path_tool_context_messages_preserved() {
    let mut session = helper_session();
    session.append_message(MessageRole::User, "写代码");
    session.append_message(MessageRole::Assistant, "好的");
    // 模拟完成度检查提示（tool_context 类型，无 tool_call_id）
    session.messages.push(tool_context_msg(
        "react_completion_check",
        "上方回复被判定为未完成",
    ));
    session.append_message(MessageRole::Assistant, "代码已完成");
    rebuild_session(&mut session);
    let context = session.context();
    // system + 4 条消息
    assert_eq!(context.len(), 5);
    // 第 3 条（index 3）应为 tool context
    let tc = &context[3];
    assert_eq!(tc.role, MessageRole::Tool);
    assert!(tc.tool_call_id.is_none());
    assert_eq!(tc.tool_name.as_deref(), Some("react_completion_check"));
    assert!(tc.content.contains("未完成"));
}

#[test]
fn new_path_end_to_end_system_prompt_sent_to_llm() {
    let mock = CapturingMockLlmServer::start("这是 LLM 的回复");
    let client = mock_client(mock.base_url());
    let mut session = multi_turn_session();
    // 不设 summary_up_to，保留所有消息以验证 tool_calls 传递
    rebuild_session(&mut session);
    let req = ModelRequest {
        session_title: session.title.clone(),
        user_input: "继续".to_string(),
        context: session.context(),
        thinking: None,
        include_media: false,
    };
    let result = client.complete(&req).expect("请求应成功");
    assert!(!result.text.is_empty());
    // 检查发送到 mock 的请求体
    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 1, "应有 1 个请求");
    let body: serde_json::Value = serde_json::from_str(&requests[0]).expect("请求体应为 JSON");
    // 验证 system prompt 包含完整内容
    let system = body["messages"]
        .as_array()
        .expect("应有 messages")
        .iter()
        .find(|m| m["role"].as_str() == Some("system"))
        .expect("应有 system 消息");
    let system_content = system["content"].as_str().expect("system 应有 content");
    assert!(
        system_content.contains("天工智能助手"),
        "system prompt 应包含身份段"
    );
    assert!(
        system_content.contains("规则"),
        "system prompt 应包含规则段"
    );
    // 验证对话消息包含 tool result
    let messages = body["messages"].as_array().unwrap();
    let tool_results = messages
        .iter()
        .filter(|m| m["role"].as_str() == Some("tool"))
        .count();
    assert!(
        tool_results >= 1,
        "应有 tool result 消息，实际: {messages:?}"
    );
    // 验证 user_input 在末尾
    let last_user = messages.last().expect("应有消息");
    assert_eq!(last_user["role"].as_str(), Some("user"));
    assert_eq!(last_user["content"].as_str(), Some("继续"));
}

#[test]
fn new_path_rebuild_after_compression_refreshes_summary() {
    let mock = MockLlmServer::start("第一次压缩摘要");
    let client = mock_client(mock.base_url());
    let mut session = multi_turn_session();
    // 第一次压缩
    let compressor = ContextCompressor::new(2);
    let result = compressor
        .update_summary_with_usage(&mut session, &client)
        .expect("压缩不应失败");
    assert!(result.compressed);
    assert!(session.context_summary.is_some());
    // 压缩后重建 system prompt
    rebuild_session(&mut session);
    let context = session.context();
    // system prompt 应包含摘要
    assert_eq!(context[0].role, MessageRole::System);
    assert!(
        context[0].content.contains("第一次压缩摘要"),
        "重建后的 system prompt 应包含最新摘要"
    );
}

#[test]
fn new_path_clear_and_rebuild_system_prompt() {
    let mut session = multi_turn_session();
    rebuild_session(&mut session);
    // 验证 system prompt 已创建
    assert!(session.system_prompt_message.is_some());
    let context_before = session.context();
    assert_eq!(context_before[0].role, MessageRole::System);
    // 模拟清空上下文
    let total = session.messages.len();
    session.summary_up_to = total;
    session.context_summary = None;
    // 重建
    rebuild_session(&mut session);
    let context_after = session.context();
    // system prompt 仍在，但对话消息为空
    assert_eq!(context_after[0].role, MessageRole::System);
    assert_eq!(context_after.len(), 1, "清空后只有 system prompt message");
    // 摘要段不应出现
    assert!(
        !context_after[0].content.contains("此前对话摘要"),
        "清空后 system prompt 不应包含摘要段"
    );
}
