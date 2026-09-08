use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tiangong_core::{
    agent_input::{AgentInput, AgentInputKind},
    core::{Plugin, TiangongCore},
    core_config::{CoreConfig, CoreConfigProvider},
    permission::TrustMode,
    session::Session,
};
use tiangong_plugin_runtime::{
    PluginRuntimeConfig, SidecarConnection, WasmPluginAdapter, WasmPluginLoader,
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

struct ToggleSidecar {
    available: AtomicBool,
    revision: AtomicUsize,
    calls: Mutex<Vec<String>>,
}
impl ToggleSidecar {
    fn new() -> Self {
        Self {
            available: AtomicBool::new(true),
            revision: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
        }
    }
}
impl SidecarConnection for ToggleSidecar {
    fn invoke(&self, operation: &str, _: &str) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push(operation.into());
        anyhow::ensure!(self.available.load(Ordering::SeqCst), "工具服务暂时不可用");
        let description = format!("revision-{}", self.revision.load(Ordering::SeqCst));
        Ok(match operation {
            "mcp.list_tools" => json!({"servers":[{"server":"probe","tools":[{"name":"read","description":description,"input_schema":{"type":"object"}}]}]}),
            "get_skill_summary" => json!({"storage_root":"/tmp/skills","items":[{"id":"probe","name":"Probe","description":description}]}),
            "mcp.execute_tool" => json!({"ok":true,"summary":"完成","stdout":"OK","stderr":"","exit_code":0,"duration_ms":1,"tool_name":"mcp::probe::read","arguments":[]}),
            _ => json!({}),
        }.to_string())
    }
}

fn adapter(id: &str, sidecar: Arc<ToggleSidecar>) -> Arc<WasmPluginAdapter> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/wasm32-wasip2/debug/tiangong_plugin_{id}_wasm.wasm"
    ));
    assert!(path.exists(), "先构建 {id} WASM: {}", path.display());
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::with_sidecar(&config, Some(sidecar)).unwrap();
    Arc::new(WasmPluginAdapter::new(
        loader.load_for_plugin(&path, &config, id).unwrap(),
        config,
    ))
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
            json!({"role":"assistant","tool_calls":[{"id":format!("call-{index}"),"type":"function","function":{"name":"mcp__probe__read","arguments":"{\"path\":\"probe\"}"}}]}),
            "tool_calls",
        ),
        None => (json!({"role":"assistant","content":"完成"}), "stop"),
    };
    ResponseTemplate::new(200)
        .set_body_json(json!({"id":"reply","choices":[{"message":message,"finish_reason":finish}]}))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_wasm_declarations_survive_core_recreation_and_refresh_on_reset() {
    for id in ["mcp", "skill"] {
        let sidecar = Arc::new(ToggleSidecar::new());
        let root = tempfile::tempdir().unwrap();
        let server = MockServer::builder().start().await;
        Mock::given(method("POST"))
            .respond_with(reply(None))
            .mount(&server)
            .await;
        let (core, config, rx, sid) = core(root.path(), &server, adapter(id, sidecar.clone()));
        for (round, available) in [true, false, true].into_iter().enumerate() {
            sidecar.available.store(available, Ordering::SeqCst);
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
        let operation = if id == "mcp" {
            "mcp.list_tools"
        } else {
            "get_skill_summary"
        };
        assert_eq!(
            sidecar
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call.as_str() == operation)
                .count(),
            1
        );
        core.shutdown_join().unwrap();

        sidecar.available.store(false, Ordering::SeqCst);
        sidecar.revision.store(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let restored = TiangongCore::builder()
            .session_id(sid.clone())
            .storage_root(root.path().to_path_buf())
            .workspace_dir(root.path().to_string_lossy())
            .trust_mode(TrustMode::FullTrust)
            .config(CoreConfigProvider::new(config))
            .stream_tx(tx)
            .plugins(vec![adapter(id, sidecar.clone()) as Arc<dyn Plugin>])
            .build();
        send(&restored, &rx, "重启后继续").await;
        assert_eq!(
            sidecar
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
        sidecar.available.store(true, Ordering::SeqCst);
        restored.deliver(AgentInputKind::reset_context()).unwrap();
        let refreshed = Session::load_from_storage(root.path(), &sid).unwrap();
        assert_ne!(
            serde_json::to_value(&before.plugin_declarations).unwrap(),
            serde_json::to_value(&refreshed.plugin_declarations).unwrap()
        );
        send(&restored, &rx, "整理后继续").await;
        let requests = server.received_requests().await.unwrap();
        let next: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        if id == "mcp" {
            assert_ne!(first["tools"], next["tools"]);
        } else {
            assert_ne!(first["messages"][0], next["messages"][0]);
        }
        restored.shutdown_join().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_execution_and_retry_do_not_change_declarations_or_invent_feedback() {
    let sidecar = Arc::new(ToggleSidecar::new());
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
    let (core, _, rx, id) = core(root.path(), &server, adapter("mcp", sidecar.clone()));
    send(&core, &rx, "初始化").await;
    sidecar.available.store(false, Ordering::SeqCst);
    send(&core, &rx, "调用工具").await;
    sidecar.available.store(true, Ordering::SeqCst);
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
        adapter("mcp", Arc::new(ToggleSidecar::new())),
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
    let sidecar = Arc::new(ToggleSidecar::new());
    sidecar.available.store(false, Ordering::SeqCst);
    let (core, _, rx, sid) = core(root.path(), &server, adapter("mcp", sidecar.clone()));
    send(&core, &rx, "离线开始").await;
    let before = Session::load_from_storage(root.path(), &sid).unwrap();
    assert!(
        !before
            .plugin_declarations
            .unwrap()
            .iter()
            .any(|item| item.plugin_id == "mcp")
    );
    sidecar.available.store(true, Ordering::SeqCst);
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
