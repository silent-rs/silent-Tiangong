//! 父 Core 中的 Agent Team 插件适配层。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

use crate::child_runtime::ChildPluginFactory;
use crate::constants::{PLUGIN_ID, TOOL_CREATE_AGENT, TOOL_DISMISS_AGENT};
use crate::coordinator::Coordinator;
use crate::tools::{error_result, root_tool_specs};

pub struct AgentTeamPlugin {
    coordinator: Arc<Coordinator>,
    feedback: RwLock<Option<PluginFeedbackTx>>,
}

impl AgentTeamPlugin {
    pub fn new(storage_root: PathBuf, child_plugins: Arc<dyn ChildPluginFactory>) -> Self {
        Self {
            coordinator: Coordinator::new(storage_root, child_plugins),
            feedback: RwLock::new(None),
        }
    }

    fn feedback(&self) -> Option<PluginFeedbackTx> {
        self.feedback
            .read()
            .ok()
            .and_then(|feedback| feedback.clone())
    }
}

impl ToolSpecProvider for AgentTeamPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        root_tool_specs()
    }
}

impl ToolOverrideHandler for AgentTeamPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        if !root_tool_specs().iter().any(|spec| spec.name == call.name) {
            return Box::pin(async { None });
        }
        if call.name == TOOL_CREATE_AGENT {
            let result = self.coordinator.create_agent(call, session);
            return Box::pin(async move { Some(result) });
        }
        if call.name == TOOL_DISMISS_AGENT {
            return match self.coordinator.prepare_dismiss_agent(call, session) {
                Ok(pending) => Box::pin(async move { Some(pending.finish().await) }),
                Err(error) => {
                    Box::pin(async move { Some(error_result(TOOL_DISMISS_AGENT, error)) })
                }
            };
        }
        let coordinator = Arc::clone(&self.coordinator);
        let call = call.clone();
        let actor_id = actor_id.to_string();
        let feedback = self.feedback();
        Box::pin(async move {
            let Some(feedback) = feedback else {
                return Some(error_result(&call.name, "Agent Team 反馈通道不可用"));
            };
            Some(coordinator.handle_tool(call, actor_id, feedback).await)
        })
    }
}

impl PromptSectionProvider for AgentTeamPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        vec![format!(
            "团队协作能力（可选）：\n\
             - create_agent 创建由独立 TiangongCore 承载的成员，最多 8 个；成员使用与当前 Core 相同的插件能力。\n\
             - send_message 会投递到目标子 Core，并等待其外部 Done/Error 终态后返回。\n\
             - 用户输入中的 @role 应调用 send_message，@all 应调用 broadcast_message；Core 本身不解析或改写 @ 消息。\n\
             - 子 Agent 向 main 只能异步报告；同级等待只允许沿创建顺序向后。\n\
             - 子 Agent 修改文件前必须加锁，命令必须前台、有限时。\n{}",
            self.coordinator.roster_prompt()
        )]
    }
}

impl Plugin for AgentTeamPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn set_workspace(&self, workspace: Option<&Path>) {
        self.coordinator.set_workspace(workspace);
    }

    fn set_feedback_tx(&self, feedback: PluginFeedbackTx) {
        if let Ok(mut current) = self.feedback.write() {
            *current = Some(feedback.clone());
        }
        self.coordinator.set_feedback(feedback);
    }

    fn on_config_updated(&self, config: &tiangong_core::core_config::CoreConfig) {
        self.coordinator.update_config(config);
    }

    fn on_session_ready(&self, session: &mut Session) {
        self.coordinator.initialize(session);
    }

    fn on_cancel<'a>(
        &'a self,
        _session: &mut Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            coordinator.cancel_all_running();
        })
    }

    fn on_session_ended(&self, session: &mut Session) {
        // Core 即将退出，销毁团队（关闭子 Core、清空 runtimes）。
        // coordinator.shutdown 是 async，但 on_session_ended 是同步钩子——
        // fire-and-forget spawn 后台执行，不阻塞 worker 退出。
        let coordinator = self.coordinator.clone();
        let session_id = session.id.clone();
        tokio::spawn(async move {
            tracing::info!(session_id, "后台销毁 Agent Team 团队");
            coordinator.shutdown().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};
    use tiangong_core::agent_input::{AgentInput, AgentInputKind};
    use tiangong_core::core::{Plugin, TiangongCore};
    use tiangong_core::core_config::{CoreConfig, CoreConfigProvider, ModelEndpoint};
    use tiangong_core::permission::TrustMode;
    use tiangong_core::session::Session;
    use tiangong_types::{ContentBlock, MessagePhase, MessageRole, StreamEvent};

    use crate::test_support::storage_test_guard;
    use crate::{build_plugin, ChildPluginFactory};

    struct ParentChildSseServer {
        base_url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl ParentChildSseServer {
        fn start() -> Self {
            Self::start_with(vec![
                tool_calls_payload(),
                text_payload("子 Agent 返回：检查通过。", 7, 3),
                text_payload("主 Agent 已读取子 Agent 的检查结果并完成最终回复。", 13, 5),
            ])
        }

        fn start_with(responses: Vec<Value>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = Arc::clone(&requests);
            let responses = Arc::new(responses);
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let connection_requests = Arc::clone(&thread_requests);
                    let connection_responses = Arc::clone(&responses);
                    std::thread::spawn(move || {
                        // macOS/BSD 可能让 accept 出来的 socket 继承监听器的非阻塞
                        // 标志。显式恢复阻塞模式，避免服务线程先于请求正文到达时把
                        // WouldBlock 当成传输失败并提前断开连接。
                        if stream.set_nonblocking(false).is_err() {
                            return;
                        }
                        if stream
                            .set_read_timeout(Some(Duration::from_secs(10)))
                            .is_err()
                        {
                            return;
                        }
                        let Ok(request) = read_json_request(&mut stream) else {
                            return;
                        };
                        let request_index = {
                            let mut recorded = connection_requests
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner());
                            let index = recorded.len();
                            recorded.push(request);
                            index
                        };
                        let payload = connection_responses
                            .get(request_index)
                            .cloned()
                            .unwrap_or_else(|| text_payload("unexpected request", 1, 1));
                        let _ = write_sse_response(&mut stream, &payload);
                    });
                }
            });
            Self {
                base_url: format!("http://{address}/v1"),
                requests,
                stop,
                thread: Some(thread),
            }
        }

        fn requests(&self) -> Vec<Value> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for ParentChildSseServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn read_json_request(stream: &mut std::net::TcpStream) -> Result<Value, String> {
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
            let read = stream
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                return Err("HTTP 请求在响应头结束前关闭".to_string());
            }
            request.extend_from_slice(&buffer[..read]);
        };
        let headers =
            std::str::from_utf8(&request[..header_end]).map_err(|error| error.to_string())?;
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .ok_or_else(|| "HTTP 请求缺少 Content-Length".to_string())?;
        while request.len() < header_end + content_length {
            let read = stream
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                return Err("HTTP 请求正文未完整发送".to_string());
            }
            request.extend_from_slice(&buffer[..read]);
        }
        serde_json::from_slice(&request[header_end..header_end + content_length])
            .map_err(|error| error.to_string())
    }

    fn tool_calls_payload() -> Value {
        let create_arguments = json!({
            "role": "reviewer",
            "label": "Reviewer",
            "system_prompt": "检查任务并返回明确结果"
        });
        let send_arguments = json!({
            "to": "reviewer",
            "content": "请检查当前任务并返回结果"
        });
        json!({
            "id": "chatcmpl-parent-tools",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call-01-create",
                            "type": "function",
                            "function": {
                                "name": "create_agent",
                                "arguments": create_arguments.to_string()
                            }
                        },
                        {
                            "index": 1,
                            "id": "call-02-send",
                            "type": "function",
                            "function": {
                                "name": "send_message",
                                "arguments": send_arguments.to_string()
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 4,
                "total_tokens": 14
            }
        })
    }

    fn text_payload(content: &str, prompt_tokens: usize, completion_tokens: usize) -> Value {
        json!({
            "id": "chatcmpl-text",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": { "content": content },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens + completion_tokens
            }
        })
    }

    fn write_sse_response(
        stream: &mut std::net::TcpStream,
        payload: &Value,
    ) -> std::io::Result<()> {
        let body = format!("data: {payload}\n\ndata: [DONE]\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes())?;
        stream.flush()
    }

    fn offers_tool(request: &Value, tool_name: &str) -> bool {
        request["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|tool| tool["function"]["name"].as_str() == Some(tool_name))
    }

    #[test]
    fn parent_continues_after_synchronous_child_turn_and_emits_one_final_done() {
        let _guard = storage_test_guard();
        let storage = tempfile::tempdir().unwrap();
        let server = ParentChildSseServer::start();
        let mut config = CoreConfig::default();
        config.llm.chat = ModelEndpoint {
            base_url: server.base_url.clone(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            timeout_ms: 5_000,
            ..Default::default()
        };
        config.trust_mode = TrustMode::FullTrust;
        config.default_trust_mode = TrustMode::FullTrust;

        let mut parent_session = Session::new("Parent");
        parent_session.cwd = storage.path().to_string_lossy().into_owned();
        parent_session.trust_mode = TrustMode::FullTrust;
        parent_session.bind_storage_root(storage.path());
        parent_session.try_persist_to_disk().unwrap();
        let child_factory: Arc<dyn ChildPluginFactory> = Arc::new(Vec::<Arc<dyn Plugin>>::new);
        let plugin = build_plugin(storage.path().to_path_buf(), child_factory);
        let (event_tx, event_rx) = std::sync::mpsc::channel::<StreamEvent>();
        let parent_core = TiangongCore::builder()
            .config(CoreConfigProvider::new(config))
            .workspace_dir(parent_session.cwd.clone())
            .session_id(parent_session.id)
            .stream_tx(event_tx)
            .plugins(vec![plugin])
            .storage_root(storage.path())
            .trust_mode(parent_session.trust_mode)
            .build();
        parent_core
            .deliver(AgentInputKind::prepared_with_id(
                "parent-turn",
                vec![ContentBlock::text(
                    "创建检查 Agent，让它完成检查，再根据结果回复我",
                )],
            ))
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(8);
        let mut events = Vec::new();
        let mut completed_at = None;
        while Instant::now() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => events.push(event),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let request_count = server.requests.lock().unwrap().len();
            let done_count = events
                .iter()
                .filter(|event| matches!(event, StreamEvent::Done { .. }))
                .count();
            if request_count >= 3 && done_count >= 1 {
                let settled = completed_at.get_or_insert_with(Instant::now);
                if settled.elapsed() >= Duration::from_millis(200) {
                    break;
                }
            }
        }
        parent_core.shutdown_join().unwrap();

        let requests = server.requests();
        let parent_requests = requests
            .iter()
            .filter(|request| offers_tool(request, "create_agent"))
            .collect::<Vec<_>>();
        assert_eq!(
            requests.len(),
            3,
            "应为父、子、父三次模型请求，实际事件：{events:#?}"
        );
        assert_eq!(parent_requests.len(), 2, "父模型必须在子轮次结束后再次请求");

        let second_parent_messages = parent_requests[1]["messages"].as_array().unwrap();
        let child_tool_result = second_parent_messages.iter().find(|message| {
            message["role"].as_str() == Some("tool")
                && message["tool_call_id"].as_str() == Some("call-02-send")
        });
        assert!(
            child_tool_result.is_some_and(|message| message["content"]
                .as_str()
                .is_some_and(|content| content.contains("子 Agent 返回：检查通过。"))),
            "父模型第二次请求必须包含 send_message 返回的子 Agent 结果"
        );

        let parent_done = events
            .iter()
            .filter(|event| matches!(event, StreamEvent::Done { .. }))
            .count();
        assert_eq!(
            parent_done, 1,
            "父会话只能在最终回复完成后发送一个 Done，实际事件：{events:#?}"
        );
        let descriptor_index = events
            .iter()
            .position(|event| {
                matches!(
                    &event,
                    StreamEvent::SessionMessageUpsert { message, .. }
                        if message.role == MessageRole::System
                            && message.text_content().contains("[Agent] Reviewer (reviewer) 已加入团队")
                )
            })
            .expect("创建后应立即同步 Agent 描述消息");
        let running_index = events
            .iter()
            .position(|event| {
                matches!(
                    &event,
                    StreamEvent::AgentStatusChanged { label, status, .. }
                        if label == "Reviewer" && status == "running"
                )
            })
            .expect("子 Agent 执行时应同步 running 状态");
        assert!(
            descriptor_index < running_index,
            "前端必须先拿到 Agent 描述，再接收运行状态和过程事件"
        );
        assert!(events.iter().any(|event| {
            matches!(
                &event,
                StreamEvent::SessionMessageUpsert { message, .. }
                    if message.phase == MessagePhase::Summary
                        && message.text_content().contains("主 Agent 已读取子 Agent 的检查结果")
            )
        }));
    }
}
