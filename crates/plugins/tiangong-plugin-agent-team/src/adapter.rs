//! 父 Core 中的 Agent Team 插件适配层。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tiangong_core::core::command::Command;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::permission::{PermissionLevel, TrustModeHandle};
use tiangong_core::session::{PendingPluginDelivery, Session};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};
use tiangong_types::ContentBlock;

use crate::child_runtime::ChildPluginFactory;
use crate::constants::{PLUGIN_ID, TOOL_CREATE_AGENT, TOOL_DISMISS_AGENT};
use crate::coordinator::Coordinator;
use crate::tools::{error_result, root_tool_specs};

pub struct AgentTeamPlugin {
    coordinator: Arc<Coordinator>,
    feedback: RwLock<Option<PluginFeedbackTx>>,
    trust_mode: RwLock<Option<TrustModeHandle>>,
}

impl AgentTeamPlugin {
    pub fn new(storage_root: PathBuf, child_plugins: Arc<dyn ChildPluginFactory>) -> Self {
        Self {
            coordinator: Coordinator::new(storage_root, child_plugins),
            feedback: RwLock::new(None),
            trust_mode: RwLock::new(None),
        }
    }

    fn feedback(&self) -> Option<PluginFeedbackTx> {
        self.feedback
            .read()
            .ok()
            .and_then(|feedback| feedback.clone())
    }

    fn current_trust_mode(&self) -> tiangong_core::permission::TrustMode {
        self.trust_mode
            .read()
            .ok()
            .and_then(|trust| trust.as_ref().map(TrustModeHandle::current))
            .unwrap_or_default()
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
        let feedback = self.feedback().map(PluginFeedbackTx::for_current_turn);
        let trust_mode = self.current_trust_mode();
        Box::pin(async move {
            let Some(feedback) = feedback else {
                return Some(error_result(&call.name, "Agent Team 反馈通道不可用"));
            };
            Some(
                coordinator
                    .handle_tool(call, actor_id, feedback, trust_mode)
                    .await,
            )
        })
    }
}

impl PromptSectionProvider for AgentTeamPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        vec![format!(
            "团队协作能力（可选）：\n\
             - create_agent 创建由独立 TiangongCore 承载的成员，最多 8 个；成员使用与当前 Core 相同的插件能力。\n\
             - send_message 会投递到目标子 Core，并等待其外部 Done/Error 终态后返回。\n\
             - 用户输入中的 @role / @all 会由插件直接可靠投递，不要再次调用 send_message。\n\
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

    fn set_trust_mode(&self, trust_mode: TrustModeHandle) {
        if let Ok(mut current) = self.trust_mode.write() {
            *current = Some(trust_mode.clone());
        }
        self.coordinator.set_trust_mode(trust_mode);
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

    fn plan_plugin_deliveries(
        &self,
        actor_id: &str,
        source_message_id: &str,
        prepared: &[ContentBlock],
    ) -> Vec<PendingPluginDelivery> {
        self.coordinator
            .plan_deliveries(actor_id, source_message_id, prepared)
    }

    fn dispatch_plugin_deliveries(&self, session: &Session, source_message_id: &str) -> bool {
        self.coordinator
            .dispatch_deliveries(session, source_message_id, self.current_trust_mode())
    }

    fn handle_runtime_command(&self, command: &Command) -> bool {
        self.coordinator.handle_runtime_command(command)
    }

    fn shutdown<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move { self.coordinator.shutdown().await })
    }

    fn tool_permission_overrides(&self) -> std::collections::BTreeMap<String, PermissionLevel> {
        root_tool_specs()
            .into_iter()
            .map(|spec| (spec.name, PermissionLevel::Safe))
            .collect()
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
    use tiangong_core::core::{CoreStorageLocation, Plugin, TiangongCore};
    use tiangong_core::core_config::{CoreConfig, CoreConfigProvider, ModelEndpoint};
    use tiangong_core::permission::TrustMode;
    use tiangong_core::session::{Message, PendingPluginDelivery, Session};
    use tiangong_types::{
        ContentBlock, MessagePhase, MessageRole, SessionStreamEvent, StreamEvent,
    };

    use super::AgentTeamPlugin;
    use crate::manifest::{child_root, team_root, AgentRecord, TeamManifest};
    use crate::state::{AgentDescriptor, AgentStatus};
    use crate::test_support::storage_test_guard;
    use crate::{build_plugin, ChildPluginFactory, PLUGIN_ID};

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
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
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
        let parent_session_id = parent_session.id.clone();
        let child_factory: Arc<dyn ChildPluginFactory> = Arc::new(Vec::<Arc<dyn Plugin>>::new);
        let plugin = build_plugin(storage.path().to_path_buf(), child_factory);
        let (event_tx, event_rx) = std::sync::mpsc::channel::<SessionStreamEvent>();
        let parent_core = TiangongCore::builder()
            .config(CoreConfigProvider::new(config))
            .session(parent_session)
            .event_sender(event_tx)
            .plugins(vec![plugin])
            .storage(CoreStorageLocation::new(storage.path()))
            .build()
            .unwrap();
        parent_core
            .deliver(AgentInputKind::prepared_with_id_and_trust_mode(
                "parent-turn",
                vec![ContentBlock::text(
                    "创建检查 Agent，让它完成检查，再根据结果回复我",
                )],
                TrustMode::FullTrust,
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
                .filter(|event| matches!(event.event, StreamEvent::Done { .. }))
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
            .filter(|event| {
                event.session_id == parent_session_id
                    && matches!(event.event, StreamEvent::Done { .. })
            })
            .count();
        assert_eq!(
            parent_done, 1,
            "父会话只能在最终回复完成后发送一个 Done，实际事件：{events:#?}"
        );
        let descriptor_index = events
            .iter()
            .position(|event| {
                matches!(
                    &event.event,
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
                    &event.event,
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
                &event.event,
                StreamEvent::SessionMessageUpsert { message, .. }
                    if message.phase == MessagePhase::Summary
                        && message.text_content().contains("主 Agent 已读取子 Agent 的检查结果")
            )
        }));
    }

    #[test]
    fn direct_mention_runs_child_before_parent_with_original_request_and_report() {
        let _guard = storage_test_guard();
        let storage = tempfile::tempdir().unwrap();
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
        let server = ParentChildSseServer::start_with(vec![
            text_payload("子 Agent 已完成直接提及任务。", 8, 3),
            text_payload(
                "主 Agent 已结合原始请求和子 Agent 报告完成最终回复。",
                14,
                5,
            ),
        ]);
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

        let mut parent_session = Session::new("Parent direct mention");
        parent_session.cwd = storage.path().to_string_lossy().into_owned();
        parent_session.trust_mode = TrustMode::FullTrust;
        parent_session.bind_storage_root(storage.path());
        let parent_session_id = parent_session.id.clone();
        let child_id = "agent-worker-direct";
        let mut manifest = TeamManifest::empty(&parent_session_id);
        let topology_order = manifest.allocate_order();
        manifest.upsert(AgentRecord {
            descriptor: AgentDescriptor {
                agent_id: child_id.to_string(),
                role: "worker".to_string(),
                label: "Worker".to_string(),
                system_prompt: "完成被直接提及的任务并返回结果".to_string(),
                status: AgentStatus::Idle,
            },
            topology_order,
        });
        manifest
            .persist(&team_root(storage.path(), &parent_session_id))
            .unwrap();

        let child_factory: Arc<dyn ChildPluginFactory> = Arc::new(Vec::<Arc<dyn Plugin>>::new);
        let concrete_plugin = Arc::new(AgentTeamPlugin::new(
            storage.path().to_path_buf(),
            child_factory,
        ));
        let plugin: Arc<dyn Plugin> = concrete_plugin;
        let (event_tx, event_rx) = std::sync::mpsc::channel::<SessionStreamEvent>();
        let parent_core = TiangongCore::builder()
            .config(CoreConfigProvider::new(config))
            .session(parent_session)
            .event_sender(event_tx)
            .plugins(vec![plugin])
            .storage(CoreStorageLocation::new(storage.path()))
            .build()
            .unwrap();
        let original_request = "@worker 请检查直接提及流程并给出结论";
        parent_core
            .deliver(AgentInputKind::prepared_with_id_and_trust_mode(
                "direct-mention-turn",
                vec![ContentBlock::text(original_request)],
                TrustMode::FullTrust,
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
                .filter(|event| matches!(event.event, StreamEvent::Done { .. }))
                .count();
            if request_count >= 2 && done_count >= 1 {
                let settled = completed_at.get_or_insert_with(Instant::now);
                if settled.elapsed() >= Duration::from_millis(200) {
                    break;
                }
            }
        }
        parent_core.shutdown_join().unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 2, "直接提及应只产生子、父两次模型请求");
        assert!(
            !offers_tool(&requests[0], "create_agent"),
            "第一次请求必须来自受限的子 Core"
        );
        assert!(
            offers_tool(&requests[1], "create_agent"),
            "第二次请求必须回到父 Core"
        );
        let final_parent_context = serde_json::to_string(&requests[1]["messages"]).unwrap();
        assert!(
            final_parent_context.contains(original_request),
            "父最终请求上下文必须保留原用户请求"
        );
        assert!(
            final_parent_context.contains("agent_team_report")
                && final_parent_context.contains("子 Agent 已完成直接提及任务。"),
            "父最终请求上下文必须包含子 Agent 报告"
        );

        let parent_done = events
            .iter()
            .filter(|event| {
                event.session_id == parent_session_id
                    && matches!(event.event, StreamEvent::Done { .. })
            })
            .count();
        assert_eq!(
            parent_done, 1,
            "父会话只能在最终回复完成后发送一个 Done，实际事件：{events:#?}"
        );
        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                StreamEvent::SessionMessageUpsert { message, .. }
                    if message.phase == MessagePhase::Summary
                        && message.text_content().contains("主 Agent 已结合原始请求")
            )
        }));
    }

    #[test]
    fn recovered_direct_mention_resumes_parent_after_child_report() {
        let _guard = storage_test_guard();
        let storage = tempfile::tempdir().unwrap();
        tiangong_core::storage::set_storage_root(storage.path().to_path_buf());
        let server = ParentChildSseServer::start_with(vec![
            text_payload("恢复后的子 Agent 已完成任务。", 9, 3),
            text_payload(
                "主 Agent 已根据恢复后的原始请求和子 Agent 报告完成回复。",
                15,
                5,
            ),
        ]);
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

        let mut parent_session = Session::new("Parent recovered direct mention");
        parent_session.cwd = storage.path().to_string_lossy().into_owned();
        parent_session.trust_mode = TrustMode::FullTrust;
        parent_session.bind_storage_root(storage.path());
        let parent_session_id = parent_session.id.clone();
        let child_id = "agent-worker-recovery";
        let unavailable_child_id = "agent-unavailable-recovery";
        let source_message_id = "recovered-direct-mention";
        let original_request = "@worker @unavailable 请恢复执行并给出完整结论";
        let delivery_id = format!("agent-team:{source_message_id}:{child_id}");
        let unavailable_delivery_id =
            format!("agent-team:{source_message_id}:{unavailable_child_id}");

        let mut source_message = Message::new(MessageRole::User, original_request);
        source_message.id = source_message_id.to_string();
        source_message.model_excluded = true;
        parent_session.messages.push(source_message);
        parent_session
            .pending_plugin_deliveries
            .push(PendingPluginDelivery {
                delivery_id: delivery_id.clone(),
                source_message_id: source_message_id.to_string(),
                plugin_id: PLUGIN_ID.to_string(),
                target_id: child_id.to_string(),
                content: original_request.to_string(),
                created_at: tiangong_core::session::now_text(),
                additional_content: vec![ContentBlock::text(original_request)],
            });
        parent_session
            .pending_plugin_deliveries
            .push(PendingPluginDelivery {
                delivery_id: unavailable_delivery_id.clone(),
                source_message_id: source_message_id.to_string(),
                plugin_id: PLUGIN_ID.to_string(),
                target_id: unavailable_child_id.to_string(),
                content: original_request.to_string(),
                created_at: tiangong_core::session::now_text(),
                additional_content: vec![ContentBlock::text(original_request)],
            });
        parent_session.try_persist_to_disk().unwrap();

        let mut manifest = TeamManifest::empty(&parent_session_id);
        let topology_order = manifest.allocate_order();
        manifest.upsert(AgentRecord {
            descriptor: AgentDescriptor {
                agent_id: child_id.to_string(),
                role: "worker".to_string(),
                label: "Worker".to_string(),
                system_prompt: "恢复执行待投递任务并返回结果".to_string(),
                status: AgentStatus::Idle,
            },
            topology_order,
        });
        let topology_order = manifest.allocate_order();
        manifest.upsert(AgentRecord {
            descriptor: AgentDescriptor {
                agent_id: unavailable_child_id.to_string(),
                role: "unavailable".to_string(),
                label: "Unavailable".to_string(),
                system_prompt: "该子 Core 用于验证恢复失败结算".to_string(),
                status: AgentStatus::Idle,
            },
            topology_order,
        });
        manifest
            .persist(&team_root(storage.path(), &parent_session_id))
            .unwrap();
        let unavailable_session_dir =
            child_root(storage.path(), &parent_session_id, unavailable_child_id).join("sessions");
        std::fs::create_dir_all(&unavailable_session_dir).unwrap();
        std::fs::write(
            unavailable_session_dir.join(format!("{unavailable_child_id}.json")),
            b"not a valid session",
        )
        .unwrap();

        let child_factory: Arc<dyn ChildPluginFactory> = Arc::new(Vec::<Arc<dyn Plugin>>::new);
        let plugin = build_plugin(storage.path().to_path_buf(), child_factory);
        let (event_tx, event_rx) = std::sync::mpsc::channel::<SessionStreamEvent>();
        let parent_core = TiangongCore::builder()
            .config(CoreConfigProvider::new(config))
            .session(parent_session)
            .event_sender(event_tx)
            .plugins(vec![plugin])
            .storage(CoreStorageLocation::new(storage.path()))
            .build()
            .unwrap();
        parent_core
            .deliver(AgentInputKind::reload_config())
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
                .filter(|event| matches!(event.event, StreamEvent::Done { .. }))
                .count();
            if request_count >= 2 && done_count >= 1 {
                let settled = completed_at.get_or_insert_with(Instant::now);
                if settled.elapsed() >= Duration::from_millis(200) {
                    break;
                }
            }
        }
        parent_core.shutdown_join().unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 2, "恢复应只产生子、父两次模型请求");
        assert!(
            !offers_tool(&requests[0], "create_agent"),
            "恢复后的第一次请求必须来自子 Core"
        );
        assert!(
            offers_tool(&requests[1], "create_agent"),
            "子报告提交后必须自动恢复父 Core"
        );
        let final_parent_context = serde_json::to_string(&requests[1]["messages"]).unwrap();
        assert!(
            final_parent_context.contains(original_request),
            "恢复后的父请求必须包含原用户消息"
        );
        assert!(
            final_parent_context.contains("agent_team_report")
                && final_parent_context.contains("恢复后的子 Agent 已完成任务。"),
            "恢复后的父请求必须包含子 Agent 报告"
        );
        assert!(
            final_parent_context.contains("Unavailable")
                && final_parent_context.contains("目标子 Core 不可用"),
            "同源中不可恢复的子 Core 必须生成失败报告，不能留下永久 pending"
        );

        let parent_done = events
            .iter()
            .filter(|event| {
                event.session_id == parent_session_id
                    && matches!(event.event, StreamEvent::Done { .. })
            })
            .count();
        assert_eq!(parent_done, 1, "恢复后的父会话只能发送一个最终 Done");
        let restored = Session::load_from_storage(storage.path(), &parent_session_id).unwrap();
        let restored_source = restored
            .messages
            .iter()
            .find(|message| message.id == source_message_id)
            .unwrap();
        assert!(!restored_source.model_excluded);
        assert!(restored.pending_plugin_deliveries.is_empty());
        assert!(restored
            .completed_plugin_delivery_ids
            .iter()
            .any(|completed| completed == &delivery_id));
        assert!(restored
            .completed_plugin_delivery_ids
            .iter()
            .any(|completed| completed == &unavailable_delivery_id));
    }
}
