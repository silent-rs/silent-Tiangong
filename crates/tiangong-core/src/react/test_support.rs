use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::agent_config::AgentConfig;
use crate::core_config::ModelEndpoint;
use crate::model::SingleProviderClient;
use crate::permission::TrustMode;
use crate::turn_context::TurnContext;

pub(crate) fn test_runtime(base_url: String) -> TurnContext {
    let client = SingleProviderClient::new(ModelEndpoint {
        base_url,
        api_key: "test-key".to_string(),
        model: "test-model".to_string(),
        timeout_ms: 5_000,
        ..Default::default()
    });
    let usage_sink = Arc::new(crate::core::plugin::TurnUsageSink::new());
    TurnContext::new(
        client,
        100_000,
        AgentConfig::default(),
        TrustMode::FullTrust,
        crate::observe::Observer::new(std::path::PathBuf::from("/tmp/tiangong-test")),
        Vec::new(),
        2,
        1,
        usage_sink,
    )
}

pub(crate) enum MockResponse {
    Text { content: String, complete: bool },
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
                MockResponse::Text { content, complete } => {
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
                            "delta": { "content": content },
                            "finish_reason": "stop"
                        }]
                    });
                    let body = format!("data: {payload}\n\n");
                    let response = if complete {
                        let body = format!("{body}data: [DONE]\n\n");
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    } else {
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
                        )
                    };
                    stream.write_all(response.as_bytes()).unwrap();
                    stream.flush().unwrap();
                    if !complete {
                        let _ = release_rx.recv_timeout(Duration::from_secs(5));
                    }
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
