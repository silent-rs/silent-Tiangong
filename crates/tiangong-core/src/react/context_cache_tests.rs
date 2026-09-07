use super::*;
use serde_json::{Value, json};

fn assert_request_prefix(previous: &Value, current: &Value) {
    for field in ["model", "instructions", "tools", "tool_choice", "reasoning"] {
        assert_eq!(previous[field], current[field], "请求的 {field} 不应漂移");
    }
    let old = previous["input"].as_array().unwrap();
    let new = current["input"].as_array().unwrap();
    assert!(new.len() >= old.len(), "历史不得缩短");
    for (index, item) in old.iter().enumerate() {
        assert_eq!(
            serde_json::to_vec(item).unwrap(),
            serde_json::to_vec(&new[index]).unwrap(),
            "第 {index} 条已发送消息必须逐字节保持一致"
        );
    }
}

async fn responses_request(server: &MockServer, index: usize) -> Value {
    let requests = server.received_requests().await.unwrap();
    serde_json::from_slice(&requests[index].body).unwrap()
}

async fn mount_responses(server: &MockServer, output: Vec<Value>, cached: u64, as_json: bool) {
    let body = json!({
        "id": format!("resp_{}", scru128::new()),
        "model": "test-model", "status": "completed", "output": output,
        "usage": {
            "input_tokens": 4000, "output_tokens": 20, "total_tokens": 4020,
            "input_tokens_details": {"cached_tokens": cached}
        }
    });
    let response = if as_json {
        ResponseTemplate::new(200).set_body_json(body)
    } else {
        let mut chunks = vec![json!({"type":"response.created", "response":{"id":body["id"]}})];
        for item in body["output"].as_array().unwrap() {
            if item["type"] == "function_call" {
                chunks.push(json!({"type":"response.output_item.added", "item":item}));
            }
        }
        chunks.push(json!({"type":"response.completed", "response":body}));
        ResponseTemplate::new(200).set_body_raw(sse_body(&chunks), "text/event-stream")
    };
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

fn answer(text: &str) -> Value {
    json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":text}]})
}

fn call(id: &str, round: usize, slow: bool, fail: bool) -> Value {
    json!({
        "type":"function_call", "call_id":id, "name":"cache_probe",
        "arguments": json!({"round":round, "slow":slow, "fail":fail,
            "nested":{"unicode":"中文\n第二行", "list":[true, null, 3]}}).to_string()
    })
}

struct CacheProbeTool {
    completed: Arc<Mutex<Vec<String>>>,
}

impl ToolOverrideHandler for CacheProbeTool {
    fn handle(
        &self,
        call: &ToolCall,
        _session: &mut Session,
        _actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
        let call = call.clone();
        let completed = self.completed.clone();
        Box::pin(async move {
            if call.arguments["slow"] == true {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            completed.lock().unwrap().push(call.id.clone());
            let failed = call.arguments["fail"] == true;
            Some(ToolResult {
                ok: !failed,
                summary: format!("结果 {}", call.id),
                stdout: format!("{}\n{}", call.id, "长结果中文\n".repeat(1500)),
                stderr: if failed {
                    "模拟执行失败".into()
                } else {
                    String::new()
                },
                exit_code: i32::from(failed),
                execution: None,
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complex_turns_preserve_every_sent_item_across_errors_injections_and_reload() {
    let server = MockServer::start().await;
    let completed = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn ToolOverrideHandler> = Arc::new(CacheProbeTool {
        completed: completed.clone(),
    });
    let spec = ToolSpec {
        name: "cache_probe".into(),
        description: "上下文一致性探针".into(),
        input_schema: json!({"type":"object", "properties":{
            "round":{"type":"integer"}, "slow":{"type":"boolean"},
            "fail":{"type":"boolean"}, "nested":{"type":"object"}
        }, "required":["round","slow","fail","nested"]}),
    };
    let mut harness = TestHarness::new_with_protocol(
        &server,
        ProviderProtocol::OpenAi,
        vec![crate::core::plugin::injection_tool_spec(), spec],
        HashMap::from([("cache_probe".to_string(), tool)]),
        Vec::new(),
    );
    let mut previous = None;
    let mut request_index = 0;
    let mut hits = 0;
    for turn in 0..4 {
        if turn > 0 {
            harness
                .ctx
                .session
                .append_message(MessageRole::User, format!("追问 {turn}"));
            // 同一用户连续追加，不能合并、替换以前发出的消息。
            harness
                .ctx
                .session
                .append_message(MessageRole::User, format!("补充条件 {turn}"));
        }
        crate::react::message::inject_tool_to_messages(
            &mut harness.ctx.session,
            "browser",
            &json!({"turn":turn, "page":"稳定页面"}),
        );
        let slow = format!("slow_{turn}");
        let fast = format!("fast_{turn}");
        mount_responses(
            &server,
            vec![
                call(&slow, turn, true, false),
                call(&fast, turn, false, turn == 1),
            ],
            1024,
            false,
        )
        .await;
        mount_responses(
            &server,
            vec![call(&format!("next_{turn}"), turn + 100, false, false)],
            2048,
            false,
        )
        .await;
        mount_responses(
            &server,
            vec![answer(&format!("第 {turn} 轮完成"))],
            3072,
            true,
        )
        .await;

        let before = serde_json::to_value(harness.ctx.session.context()).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            execute_turn(&mut harness.ctx, &mut harness.cmd_rx),
        )
        .await
        .unwrap();
        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "{result:?}"
        );
        assert_eq!(result.usage.prompt_cache_hit_tokens, Some(6144));
        assert_eq!(result.usage.prompt_cache_miss_tokens, Some(5856));
        hits += result.usage.prompt_cache_hit_tokens.unwrap();
        let after = serde_json::to_value(harness.ctx.session.context()).unwrap();
        assert!(
            after
                .as_array()
                .unwrap()
                .starts_with(before.as_array().unwrap())
        );

        for _ in 0..3 {
            let request = responses_request(&server, request_index).await;
            if let Some(old) = &previous {
                assert_request_prefix(old, &request);
            }
            assert!(
                !request.to_string().contains("已截断"),
                "展示截断不能影响模型历史"
            );
            previous = Some(request);
            request_index += 1;
        }
        {
            let completion_order = completed.lock().unwrap();
            assert!(
                completion_order.iter().position(|id| id == &fast).unwrap()
                    < completion_order.iter().position(|id| id == &slow).unwrap()
            );
        }
        let followup = responses_request(&server, request_index - 2).await;
        let ids: Vec<_> = followup["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .filter_map(|item| item["call_id"].as_str())
            .filter(|id| *id == slow || *id == fast)
            .collect();
        assert_eq!(
            ids,
            vec![slow.as_str(), fast.as_str()],
            "完成顺序不能改变调用顺序"
        );
        harness.ctx.session.try_persist_to_disk().unwrap();
        harness.ctx.session =
            Session::load_from_storage(&harness.storage_root, &harness.ctx.session.id).unwrap();
        assert_eq!(
            serde_json::to_value(harness.ctx.session.context()).unwrap(),
            after,
            "重新加载不能改变历史"
        );
        harness.drain_stream();
    }
    assert_eq!(request_index, 12);
    assert_eq!(hits, 24576);
    assert_eq!(server.received_requests().await.unwrap().len(), 12);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_steering_preserves_the_inflight_request_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_json(json!({
                    "status":"completed", "output":[answer("旧回复")]
                })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mount_responses(&server, vec![answer("已按追加要求完成")], 1024, true).await;
    let mut harness = TestHarness::new_with_protocol(
        &server,
        ProviderProtocol::OpenAi,
        Vec::new(),
        HashMap::new(),
        Vec::new(),
    );
    let cmd_tx = harness.cmd_tx.clone();
    let send = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while server.received_requests().await.unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        cmd_tx
            .send(Command::InjectUserMessage {
                message_id: scru128::new().to_string(),
                content: vec![tiangong_types::ContentBlock::text("追加要求")],
            })
            .unwrap();
    };
    let (result, _) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(execute_turn(&mut harness.ctx, &mut harness.cmd_rx), send)
    })
    .await
    .unwrap();
    assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
    assert_request_prefix(
        &responses_request(&server, 0).await,
        &responses_request(&server, 1).await,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successive_continuations_do_not_merge_previously_sent_assistant_messages() {
    let server = MockServer::start().await;
    for text in [
        "[NEED_MORE_WORK] 第一段进展",
        "[NEED_MORE_WORK] 第二段进展",
        "最终完成",
    ] {
        mount_responses(&server, vec![answer(text)], 1024, true).await;
    }
    let mut harness = TestHarness::new_with_protocol(
        &server,
        ProviderProtocol::OpenAi,
        Vec::new(),
        HashMap::new(),
        Vec::new(),
    );
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        execute_turn(&mut harness.ctx, &mut harness.cmd_rx),
    )
    .await
    .unwrap();
    assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    for index in 1..3 {
        assert_request_prefix(
            &responses_request(&server, index - 1).await,
            &responses_request(&server, index).await,
        );
    }
}
