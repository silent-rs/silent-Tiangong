use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::core::Plugin;
use crate::core::command::Command;
use crate::core_config::ModelEndpoint;
use crate::model::SingleProviderClient;
use crate::runtime::RuntimeEngine;
use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecordedRuntimeCommand {
    Approval { request_id: String, approved: bool },
    PluginControl { plugin_id: String, action: String },
}

#[derive(Default)]
pub(crate) struct RecordingPlugin {
    commands: Mutex<Vec<RecordedRuntimeCommand>>,
}

impl RecordingPlugin {
    pub(crate) fn commands(&self) -> Vec<RecordedRuntimeCommand> {
        self.commands.lock().unwrap().clone()
    }
}

impl ToolOverrideHandler for RecordingPlugin {}
impl ToolSpecProvider for RecordingPlugin {}
impl PromptSectionProvider for RecordingPlugin {}

impl Plugin for RecordingPlugin {
    fn id(&self) -> &str {
        "runtime-command-recorder"
    }

    fn handle_runtime_command(&self, command: &Command) -> bool {
        let recorded = match command {
            Command::Approval {
                request_id,
                approved,
            } => RecordedRuntimeCommand::Approval {
                request_id: request_id.clone(),
                approved: *approved,
            },
            Command::PluginControl {
                plugin_id, action, ..
            } => RecordedRuntimeCommand::PluginControl {
                plugin_id: plugin_id.clone(),
                action: action.clone(),
            },
            _ => return false,
        };
        self.commands.lock().unwrap().push(recorded);
        true
    }
}

pub(crate) fn plugin_control(action: &str) -> Command {
    Command::PluginControl {
        plugin_id: "test-plugin".to_string(),
        action: action.to_string(),
        payload: serde_json::json!({"target": "child"}),
    }
}

pub(crate) fn approval(request_id: &str, approved: bool) -> Command {
    Command::Approval {
        request_id: request_id.to_string(),
        approved,
    }
}

pub(crate) fn runtime_with_recorder(base_url: String) -> (RuntimeEngine, Arc<RecordingPlugin>) {
    let recorder = Arc::new(RecordingPlugin::default());
    let client = SingleProviderClient::new(ModelEndpoint {
        base_url,
        api_key: "test-key".to_string(),
        model: "test-model".to_string(),
        timeout_ms: 5_000,
        ..Default::default()
    });
    let engine = RuntimeEngine::for_react_test(client, recorder.clone());
    (engine, recorder)
}

pub(crate) enum MockResponse {
    Stall,
    ToolCall { name: String },
}

pub(crate) struct MockLlmServer {
    base_url: String,
    connected: Arc<(Mutex<bool>, Condvar)>,
    release: Option<std::sync::mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MockLlmServer {
    pub(crate) fn start(response: MockResponse) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let connected = Arc::new((Mutex::new(false), Condvar::new()));
        let thread_connected = connected.clone();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (lock, ready) = &*thread_connected;
            *lock.lock().unwrap() = true;
            ready.notify_all();

            match response {
                MockResponse::Stall => {
                    let _ = release_rx.recv_timeout(Duration::from_secs(5));
                }
                MockResponse::ToolCall { name } => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let payload = serde_json::json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": "test-model",
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": 0,
                                    "id": "call-test",
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": "{}"
                                    }
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }]
                    });
                    let body = format!("data: {payload}\n\ndata: [DONE]\n\n");
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                    stream.flush().unwrap();
                }
            }
        });
        Self {
            base_url: format!("http://{address}/v1"),
            connected,
            release: Some(release_tx),
            handle: Some(handle),
        }
    }

    pub(crate) fn base_url(&self) -> String {
        self.base_url.clone()
    }

    pub(crate) async fn wait_until_connected(&self) {
        let connected = self.connected.clone();
        tokio::task::spawn_blocking(move || {
            let (lock, ready) = &*connected;
            let state = lock.lock().unwrap();
            let (state, timeout) = ready
                .wait_timeout_while(state, Duration::from_secs(3), |connected| !*connected)
                .unwrap();
            assert!(*state, "模型请求未连接到测试服务：{timeout:?}");
        })
        .await
        .unwrap();
    }
}

impl Drop for MockLlmServer {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
