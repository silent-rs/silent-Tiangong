//! Core 集成测试共享基础设施：真实 `TiangongCore` 实例 + wiremock 假 LLM。
//!
//! 全部用例从 `deliver()` 发起，模型交互经 mock 服务器回放，断言落在
//! 公开可见结果上：StreamEvent 事件流与磁盘 Session 终态。

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tiangong_types::{StreamEvent, TurnStatus};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::agent_input::{AgentInput, AgentInputKind};
use crate::core::TiangongCore;
use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::permission::TrustMode;
use crate::session::Session;

pub const WAIT: Duration = Duration::from_secs(10);
pub const POLL: Duration = Duration::from_millis(10);

/// 记录调用并返回固定结果的测试工具（经插件注册进 Core）。
pub struct RecordingTool {
    pub name: &'static str,
    pub invocations: Mutex<Vec<crate::model::ToolCall>>,
    pub ok: bool,
}

impl RecordingTool {
    pub fn succeed(name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            invocations: Mutex::new(Vec::new()),
            ok: true,
        })
    }

    pub fn count(&self) -> usize {
        self.invocations.lock().unwrap().len()
    }
}

impl crate::tool_override::ToolOverrideHandler for RecordingTool {
    fn handle(
        &self,
        call: &crate::model::ToolCall,
        _session: &mut Session,
        _actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<crate::tool::ToolResult>> + Send>>
    {
        self.invocations.lock().unwrap().push(call.clone());
        let ok = self.ok;
        let name = self.name;
        Box::pin(async move {
            Some(crate::tool::ToolResult {
                ok,
                summary: format!("{name} 已执行"),
                stdout: "done".to_string(),
                stderr: String::new(),
                exit_code: i32::from(!ok),
                execution: None,
            })
        })
    }
}

/// 提供单一工具的测试插件。
pub struct ToolPlugin {
    pub id: &'static str,
    pub tool: Arc<RecordingTool>,
}

impl crate::tool_override::ToolSpecProvider for ToolPlugin {
    fn tool_specs(&self) -> Vec<crate::model::ToolSpec> {
        vec![crate::model::ToolSpec {
            name: self.tool.name.to_string(),
            description: "测试工具".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }]
    }
}

impl crate::tool_override::ToolOverrideHandler for ToolPlugin {
    fn handle(
        &self,
        call: &crate::model::ToolCall,
        session: &mut Session,
        actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<crate::tool::ToolResult>> + Send>>
    {
        self.tool.handle(call, session, actor_id)
    }
}

impl crate::tool_override::PromptSectionProvider for ToolPlugin {}
impl crate::tool_override::MentionCandidateProvider for ToolPlugin {}
impl crate::core::plugin::Plugin for ToolPlugin {
    fn id(&self) -> &str {
        self.id
    }
}

/// OpenAI SSE chunk 组装（`data: {json}` + 末尾 `[DONE]`）。
pub fn sse_body(chunks: &[serde_json::Value]) -> Vec<u8> {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {}\n\n", chunk));
    }
    body.push_str("data: [DONE]\n\n");
    body.into_bytes()
}

pub fn text_delta_chunk(content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-it",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": content},
            "finish_reason": null,
        }],
    })
}

pub fn usage_delta_chunk(prompt: u64, completion: u64) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-it",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "test-model",
        "choices": [],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt + completion,
        },
    })
}

pub fn tool_call_chunk(call_id: &str, name: &str, arguments: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-it",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments},
                }],
            },
            "finish_reason": "tool_calls",
        }],
    })
}

/// 按挂载顺序只响应一次的 SSE mock（可带延迟制造挂起窗口）。
pub async fn mount_sse(
    server: &MockServer,
    chunks: Vec<serde_json::Value>,
    delay: Option<Duration>,
) {
    let mut response =
        ResponseTemplate::new(200).set_body_raw(sse_body(&chunks), "text/event-stream");
    if let Some(delay) = delay {
        response = response.set_delay(delay);
    }
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

/// 非流式完成响应（压缩请求用）。
pub async fn mount_completion(server: &MockServer, content: &str, delay: Option<Duration>) {
    let body = serde_json::json!({
        "id": "chatcmpl-it-compress",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
    });
    let mut response = ResponseTemplate::new(200).set_body_json(body);
    if let Some(delay) = delay {
        response = response.set_delay(delay);
    }
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

/// 永久匹配的 400 请求错误（模型请求快速失败）。
pub async fn mount_permanent_error(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"message": "integration test force fail", "type": "invalid_request_error"}
        })))
        .mount(server)
        .await;
}

/// 构建真实 TiangongCore（非默认标题跳过 lite 标题生成）并保留事件接收端。
pub fn core_with(
    root: &std::path::Path,
    sid: &str,
    endpoint: &str,
    trust_mode: TrustMode,
    plugins: Vec<Arc<dyn crate::core::plugin::Plugin>>,
) -> (TiangongCore, std::sync::mpsc::Receiver<StreamEvent>) {
    let mut session = Session::new("集成测试会话".to_string());
    session.id = sid.to_string();
    session.bind_storage_root(root);
    session.try_persist_to_disk().expect("预落盘 session 失败");

    let config = CoreConfig::builder()
        .with_chat(endpoint, "test-key", "test-model")
        .with_trust_mode(trust_mode)
        .build();
    let (event_tx, event_rx) = std::sync::mpsc::channel::<StreamEvent>();
    let core = TiangongCore::builder()
        .session_id(sid.to_string())
        .config(CoreConfigProvider::new(config))
        .trust_mode(trust_mode)
        .storage_root(root)
        .workspace_dir(root.to_string_lossy())
        .stream_tx(event_tx)
        .plugins(plugins)
        .build();
    (core, event_rx)
}

/// 全信任 + 无插件的默认实例。
pub fn core_for(
    root: &std::path::Path,
    sid: &str,
    endpoint: &str,
) -> (TiangongCore, std::sync::mpsc::Receiver<StreamEvent>) {
    core_with(root, sid, endpoint, TrustMode::FullTrust, Vec::new())
}

/// 投递一条用户消息。
pub fn send_message(core: &TiangongCore, message_id: &str, text: &str) {
    core.deliver(AgentInputKind::prepared_with_id(
        message_id,
        vec![tiangong_types::ContentBlock::text(text)],
    ))
    .expect("消息投递应被接受");
}

/// 等待指定用户消息获得 turn 终态（磁盘 Session 权威）。
pub async fn wait_turn_status(root: &std::path::Path, sid: &str, message_id: &str) -> TurnStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = Session::load_from_storage(root, sid)
            .ok()
            .and_then(|session| {
                session
                    .messages
                    .iter()
                    .find(|m| m.id == message_id)
                    .and_then(|m| m.turn_status)
            })
        {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "等待消息 {message_id} 的 turn 终态超时"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// 等待 driver 回到空闲。
pub async fn wait_idle(sid: &str) {
    let deadline = Instant::now() + WAIT;
    while crate::react::inbox::is_running(sid) && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
    }
}

/// 等待 mock 服务器收到至少 `n` 个请求。
pub async fn wait_requests(server: &MockServer, n: usize) {
    let deadline = Instant::now() + WAIT;
    while server.received_requests().await.map_or(0, |r| r.len()) < n {
        assert!(Instant::now() < deadline, "等待 {n} 个模型请求超时");
        tokio::time::sleep(POLL).await;
    }
}

/// 收集当前积压的全部事件（非阻塞）。
pub fn drain_events(rx: &std::sync::mpsc::Receiver<StreamEvent>) -> Vec<StreamEvent> {
    rx.try_iter().collect()
}

/// 断言事件流中存在成功终态（Done）。
pub fn assert_done(events: &[StreamEvent]) {
    assert!(
        events.iter().any(|e| matches!(e, StreamEvent::Done { .. })),
        "事件流应包含 Done 终态，实际: {:?}",
        events.iter().map(|e| discriminant(e)).collect::<Vec<_>>()
    );
}

fn discriminant(event: &StreamEvent) -> &'static str {
    match event {
        StreamEvent::Done { .. } => "Done",
        StreamEvent::Error { .. } => "Error",
        StreamEvent::UserMessage { .. } => "UserMessage",
        StreamEvent::ApprovalNeeded { .. } => "ApprovalNeeded",
        StreamEvent::ToolStart { .. } => "ToolStart",
        StreamEvent::ToolResult { .. } => "ToolResult",
        StreamEvent::ContextCompressing { .. } => "ContextCompressing",
        StreamEvent::ContextCompressed { .. } => "ContextCompressed",
        StreamEvent::TitleChanged { .. } => "TitleChanged",
        _ => "其他",
    }
}
