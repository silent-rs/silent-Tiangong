use super::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request(session_id: &str) -> ModelRequest {
    ModelRequest {
        session_id: Some(session_id.to_string()),
        user_input: String::new(),
        context: vec![
            Message::new(MessageRole::System, "固定系统提示"),
            Message::new(MessageRole::User, "回答 OK"),
        ],
        reasoning_effort: ReasoningEffort::None,
        max_output_tokens: Some(256),
    }
}

fn reply(protocol: ProviderProtocol) -> Value {
    if protocol == ProviderProtocol::OpenAi {
        json!({"id":"resp_test","status":"completed","model":"test-model",
            "output":[{"type":"message","content":[{"type":"output_text","text":"OK"}]}],
            "usage":{"input_tokens":100,"output_tokens":2,"total_tokens":102,"input_tokens_details":{"cached_tokens":80}}})
    } else {
        json!({"id":"chat_test","created":0,"object":"chat.completion","model":"test-model",
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"OK"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":2,"total_tokens":102,"prompt_tokens_details":{"cached_tokens":80}}})
    }
}

#[tokio::test]
async fn session_headers_reach_streaming_and_non_streaming_openai_requests() {
    for protocol in [
        ProviderProtocol::OpenAi,
        ProviderProtocol::OpenAiChatCompletions,
    ] {
        let server = MockServer::start().await;
        let session_id = scru128::new().to_string();
        Mock::given(method("POST"))
            .and(header("x-opencode-session", session_id.as_str()))
            .and(header("x-custom", "unchanged"))
            .and(header(
                "user-agent",
                concat!("tiangong/", env!("CARGO_PKG_VERSION")),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(reply(protocol)))
            .expect(2)
            .mount(&server)
            .await;
        let client = SingleProviderClient::new(ModelEndpoint {
            base_url: server.uri(),
            api_key: "test-key".into(),
            model: "test-model".into(),
            protocol,
            headers: [
                ("x-opencode-session".into(), "${session_id}".into()),
                ("x-custom".into(), "unchanged".into()),
            ]
            .into(),
            ..Default::default()
        });
        let req = request(&session_id);
        let response = client.complete_async(&req).await.unwrap();
        assert_eq!(response.text, "OK");
        assert_eq!(response.usage.prompt_cache_hit_tokens, Some(80));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert_eq!(
            client
                .stream_function_calls(req, Vec::new(), tx)
                .await
                .unwrap()
                .text,
            "OK"
        );
        let requests = server.received_requests().await.unwrap();
        for request in requests {
            let payload: Value = serde_json::from_slice(&request.body).unwrap();
            if protocol == ProviderProtocol::OpenAi {
                assert_eq!(payload["prompt_cache_key"], session_id);
                assert_eq!(payload["max_output_tokens"], 256);
            } else {
                assert!(payload.get("prompt_cache_key").is_none());
                assert_eq!(payload["max_tokens"], 256);
            }
            assert!(payload.get("session_id").is_none());
        }
        server.verify().await;
    }
}

#[tokio::test]
async fn streamed_reasoning_is_replayed_verbatim_after_message_reload() {
    let server = MockServer::start().await;
    let reasoning = "  分析工具结果\n保留原文\n";
    let events = [
        json!({"choices":[{"delta":{"reasoning_content":reasoning}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"probe","type":"function","function":{"name":"read_probe","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}),
    ];
    let body = format!(
        "{}data: [DONE]\n\n",
        events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>()
    );
    Mock::given(method("POST"))
        .and(header("x-opencode-session", "reasoning_session"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(3)
        .mount(&server)
        .await;
    let client = SingleProviderClient::new(ModelEndpoint {
        base_url: server.uri(),
        api_key: "test".into(),
        model: "deepseek-v4-flash".into(),
        protocol: ProviderProtocol::OpenAiChatCompletions,
        headers: [("x-opencode-session".into(), "${session_id}".into())].into(),
        ..Default::default()
    });
    let mut req = request("reasoning_session");
    req.reasoning_effort = ReasoningEffort::High;
    let mut prefix = Vec::new();
    for _ in 0..3 {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let response = client
            .clone()
            .stream_function_calls(
                req.clone(),
                vec![ToolSpec {
                    name: "read_probe".into(),
                    description: "读取".into(),
                    input_schema: json!({"type":"object"}),
                }],
                tx,
            )
            .await
            .unwrap();
        assert_eq!(response.reasoning_content, reasoning);
        let requests = server.received_requests().await.unwrap();
        let payload: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(&messages[..prefix.len()], prefix.as_slice());
        for message in messages
            .iter()
            .filter(|message| message["role"] == "assistant")
        {
            assert_eq!(message["reasoning_content"], reasoning);
        }
        prefix = messages.clone();
        let mut assistant = Message::with_reasoning(
            MessageRole::Assistant,
            response.text,
            response.reasoning_content,
        );
        assistant.tool_calls = response
            .tool_calls
            .into_iter()
            .map(|call| tiangong_types::MessageToolCall {
                id: call.id,
                name: call.name,
                arguments: call.arguments,
            })
            .collect();
        req.context.push(assistant);
        req.context
            .push(Message::tool_result("probe", "read_probe", "OK", false));
        req.context = serde_json::from_slice(&serde_json::to_vec(&req.context).unwrap()).unwrap();
    }
}

#[tokio::test]
async fn retry_retains_the_same_session_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(header("x-opencode-session", "session_retry"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(reply(ProviderProtocol::OpenAiChatCompletions)),
        )
        .mount(&server)
        .await;
    let client = SingleProviderClient::new(ModelEndpoint {
        base_url: server.uri(),
        api_key: "test-key".into(),
        model: "test-model".into(),
        headers: [("x-opencode-session".into(), "${session_id}".into())].into(),
        ..Default::default()
    });
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    client
        .stream_function_calls(request("session_retry"), Vec::new(), tx)
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].headers.get("x-opencode-session"),
        requests[1].headers.get("x-opencode-session")
    );
    assert_eq!(requests[0].body, requests[1].body);
}

#[test]
fn provider_headers_survive_resolution_and_legacy_configs_remain_valid() {
    let config: crate::models_config::ModelsConfig = serde_json::from_value(json!({
        "providers":{"test":{"base_url":"http://localhost","api_key":"test-key","headers":{"x-opencode-session":"${session_id}"}}},
        "models":{"model":{"provider":"test","model":"test-model"}},"routing":{"chat":"model"}
    })).unwrap();
    let resolved = config
        .resolve_slot(crate::models_config::RoutingSlot::Chat)
        .unwrap();
    let endpoint = ModelEndpoint::from_resolved(resolved);
    assert_eq!(endpoint.headers["x-opencode-session"], "${session_id}");
    assert_eq!(endpoint.to_resolved().headers, endpoint.headers);
    let legacy: crate::models_config::ProviderConfig =
        serde_json::from_value(json!({"base_url":"http://localhost","api_key":"test-key"}))
            .unwrap();
    assert!(legacy.headers.is_empty());
}

#[tokio::test]
async fn deepseek_uses_user_id_and_openai_uses_prompt_cache_key() {
    for protocol in [ProviderProtocol::DeepSeek, ProviderProtocol::OpenAi] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reply(protocol)))
            .mount(&server)
            .await;
        let client = SingleProviderClient::new(ModelEndpoint {
            base_url: server.uri(),
            api_key: "test-key".into(),
            model: "test-model".into(),
            protocol,
            ..Default::default()
        });
        for id in ["session_one", "session_two"] {
            client.complete_async(&request(id)).await.unwrap();
        }
        for (request, id) in server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .zip(["session_one", "session_two"])
        {
            let payload: Value = serde_json::from_slice(&request.body).unwrap();
            let field = if protocol == ProviderProtocol::DeepSeek {
                "user_id"
            } else {
                "prompt_cache_key"
            };
            assert_eq!(payload[field], id);
            assert!(payload.get("session_id").is_none());
        }
    }
}

#[test]
fn invalid_header_values_fail_without_echoing_values() {
    let headers = [("x-custom".into(), "private-value\ninvalid".into())].into();
    let error = crate::headers::resolve_headers(&headers, "session_test")
        .unwrap_err()
        .to_string();
    assert!(!error.contains("private-value"));
    assert!(
        crate::headers::resolve_headers(
            &[("bad name".into(), "value".into())].into(),
            "session_test"
        )
        .is_err()
    );
}

#[tokio::test]
#[ignore = "需要显式指定模型并使用本机已配置的真实 API 凭据"]
async fn live_session_routing_smoke() {
    let model_key = std::env::var("TIANGONG_LIVE_MODEL_KEY").expect("设置 TIANGONG_LIVE_MODEL_KEY");
    let path =
        std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".tiangong/models.json");
    let mut config: crate::models_config::ModelsConfig =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let entry = config
        .models
        .get(&model_key)
        .expect("指定模型不存在")
        .clone();
    config
        .routing
        .insert(crate::models_config::RoutingSlot::Chat, entry);
    let mut endpoint = ModelEndpoint::from_resolved(
        config
            .resolve_slot(crate::models_config::RoutingSlot::Chat)
            .unwrap(),
    );
    endpoint.timeout_ms = 90_000;
    let client = SingleProviderClient::new(endpoint);
    let session_id = scru128::new().to_string();
    let mut req = request(&session_id);
    let thinking_tools = std::env::var_os("TIANGONG_LIVE_THINKING_TOOLS").is_some();
    if thinking_tools {
        req.reasoning_effort = ReasoningEffort::High;
        req.max_output_tokens = Some(2048);
        req.context[1] = Message::new(
            MessageRole::User,
            "调用 read_probe 工具一次，然后只回复 OK。",
        );
        let tools = vec![ToolSpec {
            name: "read_probe".into(),
            description: "返回连接检查结果".into(),
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        }];
        for round in 0..3 {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let response = tokio::time::timeout(
                Duration::from_secs(100),
                client
                    .clone()
                    .stream_function_calls(req.clone(), tools.clone(), tx),
            )
            .await
            .unwrap()
            .unwrap();
            if round == 0 {
                assert!(!response.reasoning_content.is_empty(), "应收到思考内容");
                assert!(!response.tool_calls.is_empty(), "应触发工具调用");
            }
            println!(
                "thinking tools round={}, input={}, cached={:?}, reasoning_bytes={}",
                round + 1,
                response.usage.prompt_tokens,
                response.usage.prompt_cache_hit_tokens,
                response.reasoning_content.len()
            );
            let mut assistant = Message::with_reasoning(
                MessageRole::Assistant,
                response.text,
                response.reasoning_content,
            );
            assistant.tool_calls = response
                .tool_calls
                .iter()
                .map(|call| tiangong_types::MessageToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect();
            req.context.push(assistant);
            for call in response.tool_calls {
                req.context
                    .push(Message::tool_result(call.id, call.name, "OK", false));
            }
            if round == 1 {
                let mut injection = Message::new(MessageRole::Assistant, "");
                injection.tool_calls.push(tiangong_types::MessageToolCall {
                    id: "internal_probe".into(),
                    name: "plugin_injection".into(),
                    arguments: json!({}),
                });
                req.context.push(injection);
                req.context.push(Message::tool_result(
                    "internal_probe",
                    "plugin_injection",
                    "检查已完成，只回复 OK。",
                    false,
                ));
            }
            req.context =
                serde_json::from_slice(&serde_json::to_vec(&req.context).unwrap()).unwrap();
        }
        return;
    }
    req.context[1] = Message::new(
        MessageRole::User,
        format!(
            "天工代码助手连接与缓存检查。以下是固定代码参考，忽略正文，只回复 OK。\n{}",
            (0..128)
                .map(|index| format!("fn sample_{index}(value: i32) -> i32 {{ value + 1 }}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    for round in 1..=2 {
        let response = tokio::time::timeout(Duration::from_secs(35), client.complete_async(&req))
            .await
            .unwrap()
            .unwrap();
        assert!(response.usage.prompt_tokens > 0);
        println!(
            "model={model_key}, round={round}, input={}, cached={:?}, output={}",
            response.usage.prompt_tokens,
            response.usage.prompt_cache_hit_tokens,
            response.usage.completion_tokens
        );
        req.context.push(Message::with_reasoning(
            MessageRole::Assistant,
            response.text,
            response.reasoning_content,
        ));
        req.context
            .push(Message::new(MessageRole::User, "继续，只回复 OK。"));
    }
}
