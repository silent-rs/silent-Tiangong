//! Core 端到端集成测试：真实 `TiangongCore` 实例 + prompt 路由假 LLM，
//! 全部交互从 `deliver()` 发起，断言公开可见结果（事件流 + 磁盘终态）。
//!
//! 主对话成功响应使用多帧 OpenAI SSE；路由按最新用户 prompt、工具调用结果或
//! 压缩指令选择响应，不依赖 mock 挂载顺序。压缩沿生产接口使用非流式 completion。

use std::sync::Arc;
use std::time::Duration;

use tiangong_types::TurnStatus;
use wiremock::MockServer;

use super::test_support::*;
use crate::agent_input::{AgentInput, AgentInputKind};
use crate::permission::TrustMode;
use crate::session::MessageRole;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_continuations_failure_compression_and_reload_keep_usage_balanced() {
    use crate::core_config::{CoreConfig, CoreConfigProvider};
    use crate::session::Session;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tiangong_llm::ProviderProtocol;
    use tiangong_types::TokenUsage;
    use wiremock::{Mock, ResponseTemplate, matchers::method};

    let (env, sid) = TestEnv::new("anthropic-usage");
    let server = MockServer::start().await;
    let step = AtomicUsize::new(0);
    Mock::given(method("POST")).respond_with(move |request: &wiremock::Request| {
        let index = step.fetch_add(1, Ordering::SeqCst);
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let (uncached, read, created, output) = [
            (10000,0,0,20), (20,10000,0,30), (10,10020,20,10),
            (5,10050,0,3), (15,10050,0,25),
        ][index];
        let usage = json!({"input_tokens":uncached,"cache_read_input_tokens":read,"cache_creation_input_tokens":created,"output_tokens":output});
        if body["stream"] != true {
            assert_eq!(index,4);
            return ResponseTemplate::new(200).set_body_json(json!({"id":"summary","type":"message","role":"assistant","model":"glm-5.3-flash","content":[{"type":"text","text":"[[SUMMARY]]\n已完成前两轮，第三轮请求失败。"}],"stop_reason":"end_turn","usage":usage}));
        }
        let block = if index==0 { json!({"type":"tool_use","id":"probe-call","name":"probe","input":{}}) } else { json!({"type":"text","text":"完成"}) };
        let mut events = vec![
            json!({"type":"message_start","message":{"id":format!("m{index}"),"model":"glm-5.3-flash","role":"assistant","content":[],"usage":{"input_tokens":0,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":block}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{},"usage":usage}),
        ];
        if index == 0 {
            events.insert(2, json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}));
        }
        if index==3 {
            events.push(json!({"type":"error","error":{"type":"api_error","message":"测试请求中断"}}));
        } else {
            events.push(json!({"type":"message_delta","delta":{"stop_reason":if index==0 {"tool_use"} else {"end_turn"}},"usage":usage}));
            events.push(json!({"type":"message_stop"}));
        }
        ResponseTemplate::new(200).insert_header("content-type","text/event-stream")
            .set_body_string(events.iter().map(|e|format!("event: {}\ndata: {e}\n\n",e["type"].as_str().unwrap())).collect::<String>())
    }).expect(5).mount(&server).await;

    let mut session = Session::new("GLM 用量验证");
    session.id = sid.clone();
    session.bind_storage_root(&env.root);
    // 旧累计可能包含已经没有明细的调用，不能因重新加载而抹掉。
    session.token_usage = TokenUsage {
        prompt_tokens: 17,
        completion_tokens: 5,
        total_tokens: 22,
        ..Default::default()
    };
    session.try_persist_to_disk().unwrap();
    let tool = RecordingTool::succeed("probe");
    let build = || {
        let mut config = CoreConfig::builder()
            .with_chat(&server.uri(), "test", "glm-5.3-flash")
            .with_trust_mode(TrustMode::FullTrust)
            .build();
        config.llm.chat.protocol = ProviderProtocol::Anthropic;
        let (tx, _rx) = std::sync::mpsc::channel();
        super::TiangongCore::builder()
            .session_id(sid.clone())
            .storage_root(env.root.clone())
            .workspace_dir(env.root.to_string_lossy())
            .trust_mode(TrustMode::FullTrust)
            .config(CoreConfigProvider::new(config))
            .stream_tx(tx)
            .plugins(vec![Arc::new(ToolPlugin {
                id: "usage-probe",
                tool: tool.clone(),
            })])
            .build()
    };
    let assert_totals = |expected_inputs: &[usize], expected_outputs: &[usize]| {
        let restored = env.load_session(&sid);
        let calls: Vec<_> = restored
            .messages
            .iter()
            .filter_map(|m| m.usage.as_ref())
            .collect();
        assert_eq!(
            calls
                .iter()
                .map(|u| u.tokens.prompt_tokens)
                .collect::<Vec<_>>(),
            expected_inputs
        );
        assert_eq!(
            calls
                .iter()
                .map(|u| u.tokens.completion_tokens)
                .collect::<Vec<_>>(),
            expected_outputs
        );
        let mut total = TokenUsage::default();
        for call in calls {
            assert!(call.tokens.cache_hit_rate().is_some());
            total.accumulate(&call.tokens);
        }
        assert_eq!(restored.token_usage.prompt_tokens, 17 + total.prompt_tokens);
        assert_eq!(
            restored.token_usage.completion_tokens,
            5 + total.completion_tokens
        );
        assert_eq!(restored.token_usage.total_tokens, 22 + total.total_tokens);
    };
    let core = build();
    send_message(&core, "usage-first", "查询数据");
    assert_eq!(
        wait_turn_status(&env, &sid, "usage-first").await,
        TurnStatus::Success
    );
    core.shutdown_join().unwrap();
    assert_eq!(tool.count(), 1);
    assert_totals(&[10000, 10020], &[20, 30]);
    let core = build();
    send_message(&core, "usage-second", "继续");
    assert_eq!(
        wait_turn_status(&env, &sid, "usage-second").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    assert_totals(&[10000, 10020, 10050], &[20, 30, 10]);
    assert_eq!(env.load_session(&sid).current_tokens, 10060);
    send_message(&core, "usage-failed", "再次继续");
    assert_eq!(
        wait_turn_status(&env, &sid, "usage-failed").await,
        TurnStatus::Failed
    );
    wait_idle(&sid).await;
    assert_totals(&[10000, 10020, 10050, 10055], &[20, 30, 10, 3]);
    core.deliver(AgentInputKind::compress_context()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while env.load_session(&sid).context_summary.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    core.shutdown_join().unwrap();
    assert_totals(&[10000, 10020, 10050, 10055, 10065], &[20, 30, 10, 3, 25]);
    assert_eq!(env.load_session(&sid).current_tokens, 25);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_and_tool_order_survives_core_recreation_and_followup_turns() {
    use crate::core::plugin::Plugin;
    use crate::core_config::{CoreConfig, CoreConfigProvider};
    use crate::tool_override::{
        MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct OrderedPlugin {
        id: &'static str,
        names: [&'static str; 2],
        calls: AtomicUsize,
        prompt_reads: AtomicUsize,
    }
    impl Plugin for OrderedPlugin {
        fn id(&self) -> &str {
            self.id
        }
    }
    impl MentionCandidateProvider for OrderedPlugin {}
    impl ToolOverrideHandler for OrderedPlugin {}
    impl PromptSectionProvider for OrderedPlugin {
        fn prompt_sections(&self) -> Vec<String> {
            self.prompt_reads.fetch_add(1, Ordering::SeqCst);
            vec![format!("插件提示:{}", self.id)]
        }
    }
    impl ToolSpecProvider for OrderedPlugin {
        fn tool_specs(&self) -> Vec<crate::model::ToolSpec> {
            let mut names = self.names;
            if self.calls.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
                names.reverse();
            }
            names
                .iter()
                .map(|name| crate::model::ToolSpec {
                    name: (*name).into(),
                    description: self.id.into(),
                    input_schema: serde_json::json!({"type":"object","properties":{}}),
                })
                .collect()
        }
    }

    let (env, sid) = TestEnv::new("plugin-order");
    let server = MockServer::builder().start().await;
    mount_prompt_router(
        &server,
        vec![PromptRoute::new(
            "answer",
            |_| true,
            MockReply::sse(stream_text_chunks(&["完成"])),
        )],
    )
    .await;
    let mut session = crate::session::Session::new("顺序验证");
    session.id = sid.clone();
    session.bind_storage_root(&env.root);
    session.try_persist_to_disk().unwrap();
    let mut previous: Option<serde_json::Value> = None;
    let mut system_id = None;
    // 每次重建模拟不同的插件发现顺序，每个实例内再连续发送两轮用户消息。
    for (generation, order) in [
        [0, 1, 2],
        [2, 0, 1],
        [1, 2, 0],
        [0, 2, 1],
        [1, 0, 2],
        [2, 1, 0],
    ]
    .into_iter()
    .enumerate()
    {
        let mut instances = Vec::new();
        let plugins: Vec<Arc<dyn Plugin>> = order
            .into_iter()
            .map(|index| {
                let (id, names) = [
                    ("zeta", ["a_last", "b_last"]),
                    ("prompt", ["identity_b", "identity_a"]),
                    ("alpha", ["z_first", "y_first"]),
                ][index];
                let plugin = Arc::new(OrderedPlugin {
                    id,
                    names,
                    calls: AtomicUsize::new(generation),
                    prompt_reads: AtomicUsize::new(0),
                });
                instances.push(plugin.clone());
                plugin as Arc<dyn Plugin>
            })
            .collect();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let core = super::TiangongCore::builder()
            .session_id(sid.clone())
            .config(CoreConfigProvider::new(
                CoreConfig::builder()
                    .with_chat(&server.uri(), "test-key", "test-model")
                    .with_trust_mode(TrustMode::FullTrust)
                    .build(),
            ))
            .trust_mode(TrustMode::FullTrust)
            .storage_root(env.root.clone())
            .workspace_dir(env.root.to_string_lossy())
            .stream_tx(event_tx)
            .plugins(plugins)
            .build();
        for round in 0..2 {
            let id = format!("msg-{generation}-{round}");
            send_message(&core, &id, "继续");
            assert_eq!(
                wait_turn_status(&env, &sid, &id).await,
                TurnStatus::Success,
                "events: {:?}",
                event_rx.try_iter().collect::<Vec<_>>()
            );
            wait_idle(&sid).await;
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), generation * 2 + round + 1);
            let payload: serde_json::Value =
                serde_json::from_slice(&requests.last().unwrap().body).unwrap();
            let names: Vec<_> = payload["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["function"]["name"].as_str().unwrap())
                .collect();
            assert_eq!(
                names,
                [
                    "plugin_injection",
                    "identity_a",
                    "identity_b",
                    "y_first",
                    "z_first",
                    "a_last",
                    "b_last"
                ]
            );
            let system = payload["messages"][0]["content"].as_str().unwrap();
            assert!(system.starts_with("插件提示:prompt\n\n插件提示:alpha\n\n插件提示:zeta\n\n"));
            if let Some(previous) = &previous {
                assert_eq!(previous["tools"], payload["tools"]);
                let old = previous["messages"].as_array().unwrap();
                let new = payload["messages"].as_array().unwrap();
                assert_eq!(new.len(), old.len() + 2);
                assert_eq!(&new[..old.len()], old.as_slice());
            }
            previous = Some(payload);
            let current_id = env.load_session(&sid).system_prompt_message.unwrap().id;
            if let Some(system_id) = &system_id {
                assert_eq!(system_id, &current_id);
            }
            system_id = Some(current_id);
        }
        for plugin in instances {
            let reads = usize::from(generation == 0);
            assert_eq!(plugin.calls.load(Ordering::SeqCst), generation + reads);
            assert_eq!(plugin.prompt_reads.load(Ordering::SeqCst), reads);
        }
        core.shutdown_join().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_question_completes_with_done_event() {
    let (env, sid) = TestEnv::new("plain");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![PromptRoute::new(
            "plain-answer",
            latest_user_contains("解释一下贪心算法"),
            MockReply::sse(stream_text_chunks(&["贪心算法", "是一种……"])),
        )],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-1", "解释一下贪心算法");
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-1").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    events.wait_done();
    events.assert_single_success_terminal();

    let session = env.load_session(&sid);
    assert!(
        session
            .messages
            .iter()
            .any(|message| message.text_content().contains("贪心算法是一种")),
        "分段 SSE 回复应完整保存进 session"
    );
    let request = chat_request_at(&server, 0).await;
    assert!(request.is_stream(), "普通问答必须使用流式请求");
    assert!(request.role_message_contains("user", "解释一下贪心算法"));
    routes["plain-answer"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_prompt_is_preserved_until_reset_and_failed_reset_keeps_history() {
    let (env, sid) = TestEnv::new("legacy-declarations");
    let server = MockServer::builder().start().await;
    mount_prompt_router(
        &server,
        vec![PromptRoute::new(
            "answer",
            |_| true,
            MockReply::sse(stream_text_chunks(&["完成"])),
        )],
    )
    .await;
    let (core, _) = core_for(&env, &sid, &server.uri());
    let mut legacy = env.load_session(&sid);
    legacy.system_prompt_message = Some(crate::session::Message::new(
        MessageRole::System,
        "旧系统提示，必须原样保留",
    ));
    legacy.context_summary = Some("旧摘要".into());
    legacy.try_persist_to_disk().unwrap();
    send_message(&core, "legacy-first", "继续");
    assert_eq!(
        wait_turn_status(&env, &sid, "legacy-first").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    let before = env.load_session(&sid);
    assert_eq!(
        serde_json::to_value(&before.system_prompt_message).unwrap(),
        serde_json::to_value(&legacy.system_prompt_message).unwrap()
    );
    assert_eq!(before.plugin_declarations.as_deref().unwrap().len(), 1);
    assert!(
        before.plugin_declarations.as_ref().unwrap()[0]
            .plugin_id
            .is_empty()
    );
    fail_all_persistence_for_session(&sid);
    assert!(core.deliver(AgentInputKind::reset_context()).is_err());
    assert_eq!(
        serde_json::to_value(env.load_session(&sid)).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    // 解除故障后再次清理，必须同时移除旧摘要并重建系统提示。
    clear_persistent_persistence_failure(&sid);
    core.deliver(AgentInputKind::reset_context()).unwrap();
    let reset = env.load_session(&sid);
    assert!(reset.context_summary.is_none());
    assert_eq!(reset.summary_up_to, reset.messages.len());
    assert!(
        !reset
            .system_prompt_message
            .unwrap()
            .text_content()
            .contains("旧系统提示")
    );
    core.shutdown_join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_roundtrip_executes_plugin_and_answers() {
    let (env, sid) = TestEnv::new("tool");
    // 断言请求序号时使用独立服务，避免池中旧请求在取消后迟到。
    let server = MockServer::builder().start().await;
    let tool = RecordingTool::succeed("echo");
    let plugin = Arc::new(ToolPlugin {
        id: "echo-plugin",
        tool: tool.clone(),
    });
    let routes = mount_prompt_router(
        &server,
        vec![
            PromptRoute::new(
                "tool-result-answer",
                |request| request.has_tool_result("call-1", "done"),
                MockReply::sse(stream_text_chunks(&["工具已执行，", "结果是 done。"])),
            ),
            PromptRoute::new(
                "tool-call",
                |request| {
                    request
                        .latest_user_text()
                        .is_some_and(|text| text.contains("执行 echo 工具"))
                        && request.defined_tools().iter().any(|name| name == "echo")
                        && request.tool_results().is_empty()
                },
                MockReply::sse(stream_tool_call_chunks("call-1", "echo", &["{", "}"])),
            ),
        ],
    )
    .await;
    let (core, mut events) = core_with(
        &env,
        &sid,
        &server.uri(),
        TrustMode::FullTrust,
        vec![plugin],
    );

    send_message(&core, "msg-1", "执行 echo 工具");
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-1").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    assert_eq!(tool.count(), 1, "工具应恰好执行一次");
    events.wait_done();
    events.assert_single_success_terminal();

    let first = chat_request_at(&server, 0).await;
    assert!(first.defined_tools().iter().any(|name| name == "echo"));
    assert!(first.allows_tool_calls());
    let second = chat_request_at(&server, 1).await;
    assert_eq!(
        second.assistant_tool_calls(),
        vec![("call-1".to_string(), "echo".to_string())]
    );
    let results = second.tool_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "call-1");
    assert!(results[0].1.contains("done"));
    assert_eq!(tool.call_ids(), vec!["call-1".to_string()]);
    routes["tool-call"].assert_hits(1);
    routes["tool-result-answer"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_message_aborts_and_restarts_current_turn() {
    let (env, sid) = TestEnv::new("steer");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![
            PromptRoute::new(
                "steered-answer",
                latest_user_contains("STEER-NEW-INTENT"),
                MockReply::sse(stream_text_chunks(&["按新方向", "完成。"])),
            ),
            PromptRoute::new(
                "slow-original",
                |request| {
                    request.latest_user_text().is_some_and(|text| {
                        text.contains("STEER-ORIGINAL") && !text.contains("STEER-NEW-INTENT")
                    })
                },
                MockReply::delayed_sse(
                    stream_text_chunks(&["长时间", "任务执行中"]),
                    Duration::from_secs(3),
                ),
            ),
        ],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-a", "STEER-ORIGINAL 开始一个长任务");
    wait_requests(&server, 1).await;
    send_message(&core, "msg-steer", "STEER-NEW-INTENT 换个方向处理");

    assert_eq!(
        wait_turn_status(&env, &sid, "msg-steer").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    events.wait_done();
    events.assert_single_success_terminal();
    let second = chat_request_at(&server, 1).await;
    assert!(second.role_message_contains("user", "STEER-NEW-INTENT"));
    let requests = server.received_requests().await.unwrap();
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let previous = first["messages"].as_array().unwrap();
    let current = second["messages"].as_array().unwrap();
    assert!(
        current.starts_with(previous),
        "追加用户指令不能改写已发送的消息边界"
    );
    let session = env.load_session(&sid);
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| {
                message.role == MessageRole::Assistant
                    && message.text_content().contains("按新方向完成")
            })
            .count(),
        1
    );
    assert!(!session.messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message.text_content().contains("长时间任务执行中")
            && message.phase == crate::session::MessagePhase::Summary
    }));
    routes["slow-original"].assert_hits(1);
    routes["steered-answer"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_running_turn_ends_cancelled() {
    let (env, sid) = TestEnv::new("cancel");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![PromptRoute::new(
            "cancel-slow",
            latest_user_contains("CANCEL-SLOW"),
            MockReply::delayed_sse(
                stream_text_chunks(&["长时间", "任务执行中"]),
                Duration::from_secs(3),
            ),
        )],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-1", "CANCEL-SLOW 开始一个长任务");
    wait_requests(&server, 1).await;
    core.deliver(AgentInputKind::cancel())
        .expect("取消投递应成功");

    assert_eq!(
        wait_turn_status(&env, &sid, "msg-1").await,
        TurnStatus::Cancelled
    );
    wait_idle(&sid).await;
    events.wait_cancelled();
    events.assert_single_cancelled_terminal();
    routes["cancel-slow"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_injection_deferred_into_next_request() {
    let (env, sid) = TestEnv::new("inject");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![PromptRoute::new(
            "injected-answer",
            |request| {
                request
                    .latest_user_text()
                    .is_some_and(|text| text.contains("INJECT-QUESTION"))
                    && request
                        .tool_results()
                        .iter()
                        .any(|(_, text)| text.contains("INJECT-MARK"))
            },
            MockReply::sse(stream_text_chunks(&["已结合", "页面信息回答。"])),
        )],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    core.deliver(AgentInputKind::tool(
        "browser_observation",
        serde_json::json!({"summary": "页面加载完成-INJECT-MARK", "url": "https://example.com"}),
    ))
    .expect("空闲注入应被接受");
    assert!(!crate::shared_runtime::is_running(&sid));
    assert!(server.received_requests().await.unwrap().is_empty());

    send_message(&core, "msg-1", "INJECT-QUESTION 根据页面情况回答");
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-1").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    events.wait_done();
    events.assert_single_success_terminal();
    let request = chat_request_at(&server, 0).await;
    let declared = request.assistant_tool_calls();
    assert!(request.tool_results().iter().any(|(id, text)| {
        text.contains("browser_observation")
            && text.contains("INJECT-MARK")
            && declared.iter().any(|(declared_id, _)| declared_id == id)
    }));
    routes["injected-answer"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn high_pressure_triggers_pre_request_compression() {
    let (env, sid) = TestEnv::new("compress");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![
            PromptRoute::new(
                "compression",
                |request| request.is_compression(),
                MockReply::completion("[[SUMMARY]]\n压缩后的历史摘要"),
            ),
            PromptRoute::new(
                "post-compression",
                latest_user_contains("AUTO-COMPRESS-SECOND"),
                MockReply::sse(stream_text_chunks(&["结合摘要", "回答。"])),
            ),
            PromptRoute::new(
                "pressure-source",
                latest_user_contains("AUTO-COMPRESS-FIRST"),
                MockReply::sse(vec![
                    text_delta_chunk("第一轮"),
                    text_delta_chunk("完成。"),
                    finish_chunk("stop"),
                    usage_delta_chunk(185_900, 5),
                ]),
            ),
        ],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-1", "AUTO-COMPRESS-FIRST 第一个问题");
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-1").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    send_message(&core, "msg-2", "AUTO-COMPRESS-SECOND 第二个问题");
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-2").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    events.wait_done_count(2);
    events.assert_done_count(2);

    let compression = chat_request_at(&server, 1).await;
    assert!(compression.any_message_contains("AUTO-COMPRESS-FIRST"));
    assert!(compression.any_message_contains("AUTO-COMPRESS-SECOND"));
    assert_eq!(
        compression.defined_tools(),
        chat_request_at(&server, 0).await.defined_tools()
    );
    assert!(!compression.allows_tool_calls());
    assert!(
        !compression.is_stream(),
        "压缩沿生产接口使用非流式 completion"
    );
    let post = chat_request_at(&server, 2).await;
    assert!(post.any_message_contains("压缩后的历史摘要"));
    assert!(post.role_message_contains("user", "AUTO-COMPRESS-SECOND"));
    let session = env.load_session(&sid);
    assert_eq!(session.context_summary.as_deref(), Some("压缩后的历史摘要"));
    assert!(session.summary_up_to > 0);
    routes["pressure-source"].assert_hits(1);
    routes["compression"].assert_hits(1);
    routes["post-compression"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compression_applies_summary() {
    let (env, sid) = TestEnv::new("manual-compress");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![PromptRoute::new(
            "manual-compression",
            |request| request.is_compression(),
            MockReply::completion("[[SUMMARY]]\n手动压缩摘要内容"),
        )],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    {
        let mut session = env.load_session(&sid);
        session.append_message(MessageRole::User, "第一条问题");
        session.append_message(MessageRole::User, "第二条问题");
        session.try_persist_to_disk().unwrap();
    }
    core.deliver(AgentInputKind::compress_context())
        .expect("手动压缩应被接受");
    let (event_boundary, event_remaining) =
        events.wait_context_compressed(tiangong_types::stream::ContextCompressAction::Compress);
    wait_idle(&sid).await;

    let compression = chat_request_at(&server, 0).await;
    assert!(compression.any_message_contains("第一条问题"));
    assert!(!compression.is_stream());
    let session = env.load_session(&sid);
    assert_eq!(session.context_summary.as_deref(), Some("手动压缩摘要内容"));
    assert_eq!(session.summary_up_to, event_boundary);
    assert_eq!(
        session.messages.len() - session.summary_up_to,
        event_remaining
    );
    routes["manual-compression"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn llm_failure_propagates_failed_status() {
    let (env, sid) = TestEnv::new("llm-fail");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![PromptRoute::new(
            "forced-failure",
            latest_user_contains("FORCE-LLM-FAILURE"),
            MockReply::error(400, "integration test force fail"),
        )],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-1", "FORCE-LLM-FAILURE");
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-1").await,
        TurnStatus::Failed
    );
    wait_idle(&sid).await;
    events.wait_error_containing("fail");
    events.assert_single_failure_terminal("fail");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "400 确定性失败不回退非流式重发");
    assert!(chat_request_at(&server, 0).await.is_stream());
    routes["forced-failure"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_turn_reads_latest_session() {
    let (env, sid) = TestEnv::new("next-latest");
    let server = MockServer::builder().start().await;
    let marker = format!("A-FINAL-{sid}");
    let routes = mount_prompt_router(
        &server,
        vec![
            PromptRoute::new(
                "latest-b",
                latest_user_contains("LATEST-TURN-B"),
                MockReply::sse(stream_text_chunks(&["B 的", "回答。"])),
            ),
            PromptRoute::new(
                "latest-a",
                latest_user_contains("LATEST-TURN-A"),
                MockReply::sse(stream_text_chunks(&[&marker])),
            ),
        ],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-a", "LATEST-TURN-A 第一个问题");
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-a").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    send_message(&core, "msg-b", "LATEST-TURN-B 第二个问题");
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-b").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    events.wait_done_count(2);
    events.assert_done_count(2);
    assert!(
        chat_request_at(&server, 1)
            .await
            .role_message_contains("assistant", &marker)
    );
    routes["latest-a"].assert_hits(1);
    routes["latest-b"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_during_commit_starts_next_turn_from_pending_slot() {
    let (env, sid) = TestEnv::new("commit-handoff");
    let server = MockServer::builder().start().await;
    let marker = format!("HANDOFF-A-FINAL-{sid}");
    let routes = mount_prompt_router(
        &server,
        vec![
            PromptRoute::new(
                "handoff-b",
                latest_user_contains("HANDOFF-B"),
                MockReply::sse(stream_text_chunks(&["B 的", "回答。"])),
            ),
            PromptRoute::new(
                "handoff-a",
                latest_user_contains("HANDOFF-A"),
                MockReply::sse(stream_text_chunks(&[&marker])),
            ),
        ],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    let mut finish = arm_turn_finish(&sid);
    send_message(&core, "msg-a", "HANDOFF-A 第一个问题");
    finish.wait_frozen();
    send_message(&core, "msg-b", "HANDOFF-B 第二个问题");
    assert!(
        !env.load_session(&sid)
            .messages
            .iter()
            .any(|m| m.id == "msg-b")
    );
    finish.release();

    assert_eq!(
        wait_turn_status(&env, &sid, "msg-b").await,
        TurnStatus::Success
    );
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-a").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    events.wait_done_count(2);
    events.assert_done_count(2);
    assert!(
        chat_request_at(&server, 1)
            .await
            .role_message_contains("assistant", &marker)
    );
    routes["handoff-a"].assert_hits(1);
    routes["handoff-b"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_messages_share_single_channel_without_busy() {
    let (env, sid) = TestEnv::new("busy");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![
            PromptRoute::new(
                "busy-c",
                latest_user_contains("BUSY-C"),
                MockReply::sse(stream_text_chunks(&["C ", "完成。"])),
            ),
            PromptRoute::new(
                "busy-b",
                |request| {
                    request
                        .latest_user_text()
                        .is_some_and(|text| text.contains("BUSY-B") && !text.contains("BUSY-C"))
                },
                MockReply::sse(stream_text_chunks(&["B ", "完成。"])),
            ),
            PromptRoute::new(
                "busy-a",
                latest_user_contains("BUSY-A"),
                MockReply::sse(stream_text_chunks(&["A ", "完成。"])),
            ),
        ],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    let mut finish = arm_turn_finish(&sid);
    send_message(&core, "msg-a", "BUSY-A 第一个问题");
    finish.wait_frozen();
    send_message(&core, "msg-b", "BUSY-B 第二个问题");
    let c_result = core.deliver(AgentInputKind::prepared_with_id(
        "msg-c",
        vec![tiangong_types::ContentBlock::text("BUSY-C 第三个问题")],
    ));
    assert!(c_result.is_ok(), "单通道不得因已有消息返回 Busy");
    finish.release();

    assert_eq!(
        wait_turn_status(&env, &sid, "msg-c").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    let session = env.load_session(&sid);
    assert!(
        session
            .messages
            .iter()
            .find(|message| message.id == "msg-b")
            .is_some_and(|message| message.turn_status.is_none()),
        "同一活动 turn 中被后续消息引导时，B 保留为无独立终态的意图"
    );
    events.wait_done_count(2);
    events.assert_done_count(2);
    assert!(
        env.load_session(&sid)
            .messages
            .iter()
            .any(|m| m.id == "msg-c"),
        "已接受的第三条消息必须被处理"
    );
    routes["busy-a"].assert_hits(1);
    routes["busy-b"].assert_hits(0);
    routes["busy-c"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_not_yet_saved_message_survives_shutdown() {
    let (env, sid) = TestEnv::new("survive-unsaved");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![PromptRoute::new(
            "shutdown-a",
            latest_user_contains("SHUTDOWN-A"),
            MockReply::sse(stream_text_chunks(&["A ", "完成。"])),
        )],
    )
    .await;
    let (core, _events) = core_for(&env, &sid, &server.uri());

    let mut finish = arm_turn_finish(&sid);
    send_message(&core, "msg-a", "SHUTDOWN-A 第一个问题");
    finish.wait_frozen();
    send_message(&core, "msg-b", "SHUTDOWN-B 关闭前未保存的消息");
    assert!(
        !env.load_session(&sid)
            .messages
            .iter()
            .any(|m| m.id == "msg-b")
    );

    let shutdown = std::thread::spawn(move || core.shutdown_join());
    let deadline = std::time::Instant::now() + WAIT;
    while crate::shared_runtime::is_running(&sid) {
        assert!(
            std::time::Instant::now() < deadline,
            "等待 Core 停止接收超时"
        );
        tokio::time::sleep(POLL).await;
    }
    finish.release();
    shutdown
        .join()
        .expect("关闭线程 panic")
        .expect("正常存储环境下关闭必须成功");

    let session = env.load_session(&sid);
    let pending = session
        .messages
        .iter()
        .find(|message| message.id == "msg-b")
        .expect("已接受未执行的 B 必须在关闭时保存");
    assert!(pending.turn_status.is_none(), "未执行的 B 不应有最终状态");
    routes["shutdown-a"].assert_hits(1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_next_turn_runs_after_current_turn_completes() {
    let (env, sid) = TestEnv::new("continuous-done");
    let server = MockServer::builder().start().await;
    let routes = mount_prompt_router(
        &server,
        vec![
            PromptRoute::new(
                "second-answer",
                latest_user_contains("CONTINUOUS-SECOND"),
                MockReply::sse(stream_text_chunks(&["第二轮", "完成。"])),
            ),
            PromptRoute::new(
                "first-answer",
                latest_user_contains("CONTINUOUS-FIRST"),
                MockReply::sse(stream_text_chunks(&["第一轮", "完成。"])),
            ),
        ],
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());
    let mut finish = arm_turn_finish(&sid);

    send_message(&core, "msg-first", "CONTINUOUS-FIRST 第一个任务");
    finish.wait_frozen();
    send_message(&core, "msg-second", "CONTINUOUS-SECOND 第二个任务");
    assert!(crate::shared_runtime::is_running(&sid));
    finish.release();

    assert_eq!(
        wait_turn_status(&env, &sid, "msg-second").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    events.wait_done();
    // 新语义：每轮独立终态——收尾窗口到达的排队消息接续起轮，两轮各自 Done。
    events.assert_done_count(2);
    assert!(events.seen().iter().any(
        |event| matches!(event, tiangong_types::StreamEvent::UserMessage { message_id, .. } if message_id == "msg-second")
    ));
    routes["first-answer"].assert_hits(1);
    routes["second-answer"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}
