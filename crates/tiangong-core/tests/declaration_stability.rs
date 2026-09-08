//! Core 声明稳定性测试：直接模拟 Plugin 接口，不加载 WASM 或启动插件服务。
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tiangong_core::{
    agent_input::{AgentInput, AgentInputKind},
    core::{Plugin, TiangongCore},
    core_config::{CoreConfig, CoreConfigProvider},
    model::{ToolCall, ToolSpec},
    permission::TrustMode,
    session::Session,
    tool::ToolResult,
    tool_override::{
        MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
    },
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
struct PluginState {
    available: AtomicBool,
    revision: AtomicUsize,
    calls: Mutex<Vec<String>>,
}
impl PluginState {
    fn new() -> Self {
        Self {
            available: AtomicBool::new(true),
            revision: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn declaration(&self, operation: &str) -> Result<String, String> {
        self.calls.lock().unwrap().push(operation.into());
        if !self.available.load(Ordering::SeqCst) {
            return Err("插件声明暂时不可用".into());
        }
        Ok(format!("revision-{}", self.revision.load(Ordering::SeqCst)))
    }
}
struct MockPlugin {
    id: String,
    state: Arc<PluginState>,
}
impl Plugin for MockPlugin {
    fn id(&self) -> &str {
        &self.id
    }
}
impl MentionCandidateProvider for MockPlugin {}
impl ToolSpecProvider for MockPlugin {
    fn try_tool_specs(&self) -> Result<Vec<ToolSpec>, String> {
        let description = if self.id == "dynamic-tools" {
            self.state.declaration("tools")?
        } else {
            "固定工具".into()
        };
        Ok(vec![ToolSpec {
            name: "probe_read".into(),
            description,
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
        }])
    }
}
impl PromptSectionProvider for MockPlugin {
    fn try_prompt_sections(&self) -> Result<Vec<String>, String> {
        if self.id == "dynamic-prompt" {
            Ok(vec![self.state.declaration("prompt")?])
        } else {
            Ok(vec!["固定插件提示".into()])
        }
    }
}
impl ToolOverrideHandler for MockPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        _session: &mut Session,
        _actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let handled = call.name == "probe_read";
        let ok = self.state.available.load(Ordering::SeqCst);
        Box::pin(async move {
            handled.then(|| ToolResult {
                ok,
                summary: if ok {
                    "完成"
                } else {
                    "工具暂时不可用"
                }
                .into(),
                stdout: if ok { "OK" } else { "" }.into(),
                stderr: if ok { "" } else { "工具暂时不可用" }.into(),
                exit_code: if ok { 0 } else { 1 },
                execution: None,
            })
        })
    }
}
fn plugin(id: &str, state: Arc<PluginState>) -> Arc<dyn Plugin> {
    Arc::new(MockPlugin {
        id: id.into(),
        state,
    })
}

fn core(
    root: &std::path::Path,
    server: &MockServer,
    plugin: Arc<dyn Plugin>,
) -> (
    TiangongCore,
    CoreConfig,
    std::sync::mpsc::Receiver<tiangong_types::StreamEvent>,
    String,
) {
    let mut session = Session::new("稳定声明验证");
    session.cwd = root.to_string_lossy().into_owned();
    session.bind_storage_root(root);
    session.try_persist_to_disk().unwrap();
    let config = CoreConfig::builder()
        .with_chat(&server.uri(), "test", "test-model")
        .with_trust_mode(TrustMode::FullTrust)
        .build();
    let (tx, rx) = std::sync::mpsc::channel();
    let core = TiangongCore::builder()
        .session_id(session.id.clone())
        .storage_root(root.to_path_buf())
        .workspace_dir(root.to_string_lossy())
        .trust_mode(TrustMode::FullTrust)
        .config(CoreConfigProvider::new(config.clone()))
        .stream_tx(tx)
        .plugins(vec![plugin])
        .build();
    (core, config, rx, session.id)
}

async fn send(
    core: &TiangongCore,
    rx: &std::sync::mpsc::Receiver<tiangong_types::StreamEvent>,
    text: &str,
) {
    core.deliver(AgentInputKind::prepared(vec![
        tiangong_types::ContentBlock::text(text),
    ]))
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(12), async {
        let mut done = false;
        loop {
            done |= rx
                .try_iter()
                .any(|event| matches!(event, tiangong_types::StreamEvent::Done { .. }));
            if done && !core.is_busy() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

fn reply(call: Option<usize>) -> ResponseTemplate {
    let (message, finish) = match call {
        Some(index) => (
            json!({"role":"assistant","tool_calls":[{"id":format!("call-{index}"),"type":"function","function":{"name":"probe_read","arguments":"{\"path\":\"probe\"}"}}]}),
            "tool_calls",
        ),
        None => (json!({"role":"assistant","content":"完成"}), "stop"),
    };
    ResponseTemplate::new(200)
        .set_body_json(json!({"id":"reply","choices":[{"message":message,"finish_reason":finish}]}))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declarations_survive_core_recreation_and_refresh_on_reset() {
    for id in ["dynamic-tools", "dynamic-prompt"] {
        let state = Arc::new(PluginState::new());
        let root = tempfile::tempdir().unwrap();
        let server = MockServer::builder().start().await;
        Mock::given(method("POST"))
            .respond_with(reply(None))
            .mount(&server)
            .await;
        let (core, config, rx, sid) = core(root.path(), &server, plugin(id, state.clone()));
        for (round, available) in [true, false, true].into_iter().enumerate() {
            state.available.store(available, Ordering::SeqCst);
            core.replace_config(config.clone()).unwrap();
            send(&core, &rx, &format!("继续 {round}")).await;
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
        for request in &requests[1..] {
            let next: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(first["tools"], next["tools"]);
            let old = first["messages"].as_array().unwrap();
            assert_eq!(
                &next["messages"].as_array().unwrap()[..old.len()],
                old.as_slice()
            );
        }
        let operation = if id == "dynamic-tools" {
            "tools"
        } else {
            "prompt"
        };
        assert_eq!(
            state
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call.as_str() == operation)
                .count(),
            1
        );
        core.shutdown_join().unwrap();

        state.available.store(false, Ordering::SeqCst);
        state.revision.store(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let restored = TiangongCore::builder()
            .session_id(sid.clone())
            .storage_root(root.path().to_path_buf())
            .workspace_dir(root.path().to_string_lossy())
            .trust_mode(TrustMode::FullTrust)
            .config(CoreConfigProvider::new(config))
            .stream_tx(tx)
            .plugins(vec![plugin(id, state.clone()) as Arc<dyn Plugin>])
            .build();
        send(&restored, &rx, "重启后继续").await;
        assert_eq!(
            state
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call.as_str() == operation)
                .count(),
            1
        );
        let requests = server.received_requests().await.unwrap();
        let replay: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        assert_eq!(first["tools"], replay["tools"]);
        assert_eq!(first["messages"][0], replay["messages"][0]);

        let before = Session::load_from_storage(root.path(), &sid).unwrap();
        restored.deliver(AgentInputKind::reset_context()).unwrap();
        let offline = Session::load_from_storage(root.path(), &sid).unwrap();
        assert_eq!(
            serde_json::to_value(&before.plugin_declarations).unwrap(),
            serde_json::to_value(&offline.plugin_declarations).unwrap()
        );
        state.available.store(true, Ordering::SeqCst);
        restored.deliver(AgentInputKind::reset_context()).unwrap();
        let refreshed = Session::load_from_storage(root.path(), &sid).unwrap();
        assert_ne!(
            serde_json::to_value(&before.plugin_declarations).unwrap(),
            serde_json::to_value(&refreshed.plugin_declarations).unwrap()
        );
        send(&restored, &rx, "整理后继续").await;
        let requests = server.received_requests().await.unwrap();
        let next: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        if id == "dynamic-tools" {
            assert_ne!(first["tools"], next["tools"]);
        } else {
            assert_ne!(first["messages"][0], next["messages"][0]);
        }
        restored.shutdown_join().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_execution_and_retry_do_not_change_declarations_or_invent_feedback() {
    let state = Arc::new(PluginState::new());
    let root = tempfile::tempdir().unwrap();
    let server = MockServer::builder().start().await;
    let step = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            let step = step.fetch_add(1, Ordering::SeqCst);
            reply(matches!(step, 1 | 3).then_some(step))
        })
        .mount(&server)
        .await;
    let (core, _, rx, id) = core(root.path(), &server, plugin("dynamic-tools", state.clone()));
    send(&core, &rx, "初始化").await;
    state.available.store(false, Ordering::SeqCst);
    send(&core, &rx, "调用工具").await;
    state.available.store(true, Ordering::SeqCst);
    send(&core, &rx, "重试工具").await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 5);
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let last: Value = serde_json::from_slice(&requests[4].body).unwrap();
    for request in &requests[1..] {
        let next: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(first["tools"], next["tools"]);
        assert_eq!(first["messages"][0], next["messages"][0]);
    }
    assert!(
        String::from_utf8_lossy(&requests[2].body).contains("暂时不可用"),
        "{}",
        String::from_utf8_lossy(&requests[2].body)
    );
    assert!(!last["messages"].to_string().contains("plugin_availability"));
    let session = Session::load_from_storage(root.path(), &id).unwrap();
    assert_eq!(
        session
            .messages
            .iter()
            .flat_map(|message| &message.tool_calls)
            .filter(|call| call.arguments["source"] == "plugin_availability")
            .count(),
        0
    );
    core.shutdown_join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_plugin_after_restart_keeps_tools_and_reports_execution_failure() {
    let root = tempfile::tempdir().unwrap();
    let server = MockServer::builder().start().await;
    let step = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            let step = step.fetch_add(1, Ordering::SeqCst);
            reply((step == 1).then_some(step))
        })
        .mount(&server)
        .await;
    let (core, config, rx, sid) = core(
        root.path(),
        &server,
        plugin("dynamic-tools", Arc::new(PluginState::new())),
    );
    send(&core, &rx, "初始化").await;
    core.shutdown_join().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let restored = TiangongCore::builder()
        .session_id(sid)
        .storage_root(root.path().to_path_buf())
        .workspace_dir(root.path().to_string_lossy())
        .trust_mode(TrustMode::FullTrust)
        .config(CoreConfigProvider::new(config))
        .stream_tx(tx)
        .plugins(Vec::new())
        .build();
    send(&restored, &rx, "调用原工具").await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    for request in &requests[1..] {
        let next: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(first["tools"], next["tools"]);
        assert_eq!(first["messages"][0], next["messages"][0]);
    }
    assert!(String::from_utf8_lossy(&requests[2].body).contains("未注册的工具"));
    restored.shutdown_join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undiscovered_offline_plugin_does_not_block_chat_and_is_added_on_reset() {
    let root = tempfile::tempdir().unwrap();
    let server = MockServer::builder().start().await;
    Mock::given(method("POST"))
        .respond_with(reply(None))
        .mount(&server)
        .await;
    let state = Arc::new(PluginState::new());
    state.available.store(false, Ordering::SeqCst);
    let (core, _, rx, sid) = core(root.path(), &server, plugin("dynamic-tools", state.clone()));
    send(&core, &rx, "离线开始").await;
    let before = Session::load_from_storage(root.path(), &sid).unwrap();
    assert!(
        !before
            .plugin_declarations
            .unwrap()
            .iter()
            .any(|item| item.plugin_id == "dynamic-tools")
    );
    state.available.store(true, Ordering::SeqCst);
    send(&core, &rx, "恢复后普通续聊").await;
    core.deliver(AgentInputKind::reset_context()).unwrap();
    send(&core, &rx, "整理后继续").await;
    let requests = server.received_requests().await.unwrap();
    let bodies: Vec<Value> = requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0]["tools"], bodies[1]["tools"]);
    assert_eq!(bodies[0]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(bodies[2]["tools"].as_array().unwrap().len(), 2);
    core.shutdown_join().unwrap();
}
