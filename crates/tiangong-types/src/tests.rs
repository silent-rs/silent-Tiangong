use crate::*;

#[test]
fn message_new() {
    let msg = Message::new(MessageRole::User, "你好");
    assert_eq!(msg.role, MessageRole::User);
    assert_eq!(msg.text_content(), "你好");
    assert!(!msg.id.is_empty());
    assert!(!msg.created_at.is_empty());
}

#[test]
fn message_with_reasoning() {
    let msg = Message::with_reasoning(MessageRole::Assistant, "回复", "思考过程");
    assert_eq!(msg.reasoning_content, "思考过程");
}

#[test]
fn turn_status_serde() {
    // 序列化为小写形式，与 RunStatus/MessagePhase 风格一致。
    assert_eq!(
        serde_json::to_string(&TurnStatus::Cancelled).unwrap(),
        "\"cancelled\""
    );
    assert_eq!(
        serde_json::from_str::<TurnStatus>("\"failed\"").unwrap(),
        TurnStatus::Failed
    );
}

#[test]
fn message_turn_metadata_backward_compatible() {
    // 旧 session 的用户消息不包含 elapsed_ms / turn_status，反序列化应为 None。
    let legacy = r#"{
        "id": "u1",
        "role": "user",
        "content": "你好",
        "reasoning_content": "",
        "created_at": "2026-01-01 00:00:00"
    }"#;
    let msg: Message = serde_json::from_str(legacy).unwrap();
    assert_eq!(msg.role, MessageRole::User);
    assert_eq!(msg.elapsed_ms, None);
    assert_eq!(msg.turn_status, None);
}

#[test]
fn legacy_attachment_blocks_migrate_to_ready_content() {
    let legacy = r#"{
        "id":"legacy-message",
        "role":"user",
        "content":[
            {"type":"text","text":"查看资源"},
            {"type":"attachment","attachment":{
                "asset_id":"inline-1",
                "local_path":"/tmp/inline.png",
                "original_name":"inline.png",
                "mime_type":"image/png",
                "size":4,
                "kind":"image",
                "handling_mode":"inline_image",
                "capability":"chat_multimodal",
                "capability_available":true
            }},
            {"type":"attachment","attachment":{
                "asset_id":"resource-1",
                "local_path":"/tmp/resource.png",
                "original_name":"resource.png",
                "mime_type":"image/png",
                "size":8,
                "kind":"image",
                "handling_mode":"analyze_with_plugin",
                "capability":"analyze_attachment",
                "capability_available":true
            }}
        ],
        "created_at":"2026-01-01 00:00:00"
    }"#;

    let message: Message = serde_json::from_str(legacy).unwrap();

    assert!(matches!(
        &message.content[1],
        ContentBlock::Image { asset, data: None } if asset.asset_id == "inline-1"
    ));
    assert!(matches!(
        &message.content[2],
        ContentBlock::AssetReference { asset } if asset.asset_id == "resource-1"
    ));
    assert!(matches!(
        &message.content[3],
        ContentBlock::ModelInstruction { text }
            if text.contains("message_id=legacy-message")
                && text.contains("attachment_index=1")
                && text.contains("path=/tmp/resource.png")
    ));
    let migrated_json = serde_json::to_string(&message).unwrap();
    assert!(!migrated_json.contains("handling_mode"));
    assert!(!migrated_json.contains("analyze_attachment"));
}

#[test]
fn legacy_user_media_migrates_but_assistant_media_remains_display_only() {
    let user_json = r#"{
        "id":"legacy-user",
        "role":"user",
        "content":"处理文件",
        "media":[{"kind":"file","url":"/tmp/report.pdf","title":"report.pdf"}],
        "created_at":"2026-01-01 00:00:00"
    }"#;
    let user: Message = serde_json::from_str(user_json).unwrap();
    assert!(matches!(
        &user.content[1],
        ContentBlock::AssetReference { asset } if asset.local_path == "/tmp/report.pdf"
    ));
    assert!(matches!(
        &user.content[2],
        ContentBlock::ModelInstruction { text } if text.contains("path=/tmp/report.pdf")
    ));

    let assistant_json = r#"{
        "id":"legacy-assistant",
        "role":"assistant",
        "content":"结果",
        "media":[{"kind":"image","url":"/tmp/result.png"}],
        "created_at":"2026-01-01 00:00:00"
    }"#;
    let assistant: Message = serde_json::from_str(assistant_json).unwrap();
    assert!(matches!(
        &assistant.content[1],
        ContentBlock::Media { url, .. } if url == "/tmp/result.png"
    ));
}

#[test]
fn legacy_user_local_image_remains_a_sendable_image() {
    let json = r#"{
        "id":"legacy-image",
        "role":"user",
        "content":"查看图片",
        "media":[{"kind":"image","url":"/tmp/history.png","title":"history.png"}],
        "created_at":"2026-01-01 00:00:00"
    }"#;

    let message: Message = serde_json::from_str(json).unwrap();
    assert!(matches!(
        &message.content[1],
        ContentBlock::Image { asset, data: None }
            if asset.local_path == "/tmp/history.png" && asset.mime_type == "image/png"
    ));
}

#[test]
fn legacy_user_data_url_is_redacted_during_migration() {
    let legacy_payload = "VERY_LARGE_LEGACY_BASE64_PAYLOAD";
    let json = format!(
        r#"{{
            "id":"legacy-data-url",
            "role":"user",
            "content":[
                {{"type":"text","text":"处理旧图片"}},
                {{"type":"media","kind":"image","url":"data:image/png;base64,{legacy_payload}"}},
                {{"type":"attachment","attachment":{{
                    "asset_id":"data:image/png;base64,{legacy_payload}",
                    "local_path":"data:image/png;base64,{legacy_payload}",
                    "original_name":"legacy.png",
                    "mime_type":"image/png",
                    "size":32,
                    "kind":"image",
                    "handling_mode":"inline_image"
                }}}}
            ],
            "created_at":"2026-01-01 00:00:00"
        }}"#
    );

    let message: Message = serde_json::from_str(&json).unwrap();
    let migrated = serde_json::to_string(&message).unwrap();

    assert!(!migrated.contains(legacy_payload));
    assert!(migrated.contains("legacy-inline-data-unavailable"));
    assert!(migrated.contains("重新上传"));
    assert_eq!(message.extract_stored_assets().len(), 2);
    assert!(matches!(
        &message.content[1],
        ContentBlock::AssetReference { asset }
            if asset.asset_id.starts_with("legacy-")
                && !asset.asset_id.contains(legacy_payload)
    ));
}

#[test]
fn new_content_blocks_redact_case_insensitive_inline_data_references_on_load() {
    let secret = "SECRET_CASE_INSENSITIVE_BASE64";
    let user_json = format!(
        r#"{{
            "id":"new-image-data-path",
            "role":"user",
            "content":[{{
                "type":"image",
                "asset":{{
                    "asset_id":"asset-1",
                    "local_path":"DATA:image/png;base64,{secret}",
                    "original_name":"image.png",
                    "mime_type":"image/png",
                    "size":4,
                    "kind":"image"
                }}
            }}],
            "created_at":"2026-01-01 00:00:00"
        }}"#
    );
    let assistant_json = format!(
        r#"{{
            "id":"assistant-data-media",
            "role":"assistant",
            "content":[{{"type":"media","kind":"image","url":"DaTa:image/png;base64,{secret}"}}],
            "created_at":"2026-01-01 00:00:00"
        }}"#
    );

    for json in [user_json, assistant_json] {
        let message: Message = serde_json::from_str(&json).unwrap();
        let stable_json = serde_json::to_string(&message).unwrap();
        assert!(!stable_json.contains(secret));
        assert!(stable_json.contains("inline-data-reference-unavailable"));
    }
}

#[test]
fn session_new() {
    let session = Session::new("测试");
    assert_eq!(session.title, "测试");
    assert!(session.messages.is_empty());
}

#[test]
fn session_append() {
    let mut session = Session::new("测试");
    session.append_message(MessageRole::User, "你好");
    session.append_message_with_reasoning(MessageRole::Assistant, "回复", "思考");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[1].reasoning_content, "思考");
}

#[test]
fn token_usage_accumulate() {
    let mut a = TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };
    let b = TokenUsage {
        prompt_tokens: 200,
        completion_tokens: 100,
        total_tokens: 300,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };
    a.accumulate(&b);
    assert_eq!(a.total_tokens, 450);
}

#[test]
fn token_usage_accumulate_cache_fields() {
    // 双方都有 cache 值 → 相加
    let mut a = TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
        prompt_cache_hit_tokens: Some(80),
        prompt_cache_miss_tokens: Some(20),
    };
    let b = TokenUsage {
        prompt_tokens: 200,
        completion_tokens: 100,
        total_tokens: 300,
        prompt_cache_hit_tokens: Some(60),
        prompt_cache_miss_tokens: Some(40),
    };
    a.accumulate(&b);
    assert_eq!(a.prompt_cache_hit_tokens, Some(140));
    assert_eq!(a.prompt_cache_miss_tokens, Some(60));

    // 自身为 None、对方为 Some → 取对方值（修复前的 bug：会被丢弃）
    let mut c = TokenUsage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };
    let d = TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 0,
        total_tokens: 100,
        prompt_cache_hit_tokens: Some(90),
        prompt_cache_miss_tokens: Some(10),
    };
    c.accumulate(&d);
    assert_eq!(c.prompt_cache_hit_tokens, Some(90), "None+Some 应取对方值");
    assert_eq!(c.prompt_cache_miss_tokens, Some(10), "None+Some 应取对方值");

    // 双方都为 None → 仍为 None
    let mut e = TokenUsage::default();
    let f = TokenUsage::default();
    e.accumulate(&f);
    assert_eq!(e.prompt_cache_hit_tokens, None);
    assert_eq!(e.prompt_cache_miss_tokens, None);
}

#[test]
fn run_status_serde() {
    let json = serde_json::to_string(&RunStatus::Executing).unwrap();
    assert_eq!(json, r#""executing""#);
    let parsed: RunStatus = serde_json::from_str(r#""idle""#).unwrap();
    assert_eq!(parsed, RunStatus::Idle);
}

#[test]
fn stream_event_serde() {
    let event = StreamEvent::Delta {
        message_id: "msg-1".into(),
        content: "你好".into(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains(r#""type":"delta""#));
    assert!(json.contains(r#""content":"你好""#));
    assert!(json.contains(r#""message_id":"msg-1""#));

    let done = StreamEvent::Done { usage: None };
    let json = serde_json::to_string(&done).unwrap();
    assert_eq!(json, r#"{"type":"done"}"#);

    let tool = StreamEvent::ToolCalls {
        message_id: "msg-2".into(),
        names: vec!["read_file".into(), "list_dir".into()],
        calls: Vec::new(),
        usage: None,
    };
    let json = serde_json::to_string(&tool).unwrap();
    assert!(json.contains(r#""type":"tool_calls""#));
    assert!(json.contains("read_file"));

    let elapsed = StreamEvent::TurnElapsed { seconds: 3 };
    let json = serde_json::to_string(&elapsed).unwrap();
    assert_eq!(json, r#"{"type":"turn_elapsed","seconds":3}"#);
}

#[test]
fn user_message_event_preserves_content_blocks_without_serializing_image_data() {
    let asset = StoredAsset {
        asset_id: "asset-1".into(),
        local_path: "/tmp/image.png".into(),
        original_name: "image.png".into(),
        mime_type: "image/png".into(),
        size: 4,
        kind: MediaKind::Image,
    };
    let event = StreamEvent::UserMessage {
        message_id: "msg-resource".into(),
        content: "查看资源".into(),
        content_blocks: vec![ContentBlock::Image {
            asset,
            data: Some("SECRET_BASE64".into()),
        }],
        media: Vec::new(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("content_blocks"));
    assert!(json.contains("/tmp/image.png"));
    assert!(!json.contains("SECRET_BASE64"));

    let legacy = r#"{
        "type":"user_message",
        "message_id":"legacy-message",
        "content":"legacy",
        "media":[{"kind":"image","url":"/tmp/legacy.png"}]
    }"#;
    let parsed: StreamEvent = serde_json::from_str(legacy).unwrap();
    match parsed {
        StreamEvent::UserMessage {
            content_blocks,
            media,
            ..
        } => {
            assert!(content_blocks.is_empty());
            assert_eq!(media.len(), 1);
        }
        _ => panic!("应反序列化为 UserMessage"),
    }
}

#[test]
fn stream_event_phase_variants_serde() {
    // ReAct 阶段过程性文本
    let react = StreamEvent::ReactText {
        message_id: "m1".into(),
        content: "正在处理".into(),
    };
    let json = serde_json::to_string(&react).unwrap();
    assert!(
        json.contains(r#""type":"react_text""#),
        "react_text 标签错误: {json}"
    );
    let parsed: StreamEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(parsed, StreamEvent::ReactText { .. }));

    // 总结阶段最终回复
    let summary = StreamEvent::SummaryText {
        message_id: "m2".into(),
        content: "已完成".into(),
    };
    let json = serde_json::to_string(&summary).unwrap();
    assert!(
        json.contains(r#""type":"summary_text""#),
        "summary_text 标签错误: {json}"
    );

    // 阶段切换通知
    let phase = StreamEvent::PhaseChanged {
        phase: "summary".into(),
        iteration: 1,
    };
    let json = serde_json::to_string(&phase).unwrap();
    assert_eq!(
        json,
        r#"{"type":"phase_changed","phase":"summary","iteration":1}"#
    );
}

#[test]
fn message_role_serde() {
    let json = serde_json::to_string(&MessageRole::Assistant).unwrap();
    assert_eq!(json, r#""assistant""#);
}

#[test]
fn message_phase_serde() {
    assert_eq!(
        serde_json::to_string(&MessagePhase::Normal).unwrap(),
        r#""normal""#
    );
    assert_eq!(
        serde_json::to_string(&MessagePhase::React).unwrap(),
        r#""react""#
    );
    assert_eq!(
        serde_json::to_string(&MessagePhase::Summary).unwrap(),
        r#""summary""#
    );
}

#[test]
fn message_phase_defaults_to_normal_for_legacy_messages() {
    // 旧 session 持久化的消息没有 phase 字段，反序列化时应默认为 Normal。
    // 这里手动构造一条缺失 phase 字段的旧格式消息 JSON。
    let legacy_json = r#"{
        "id": "legacy-1",
        "role": "assistant",
        "content": "旧消息",
        "reasoning_content": "",
        "created_at": "2026-01-01 00:00:00"
    }"#;
    let msg: Message = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(msg.phase, MessagePhase::Normal);
    assert_eq!(msg.text_content(), "旧消息");
}

#[test]
fn session_serde_roundtrip() {
    let mut session = Session::new("测试会话");
    session.append_message(MessageRole::User, "你好");
    session.append_message(MessageRole::Assistant, "你好！");

    let json = serde_json::to_string(&session).unwrap();
    let parsed: Session = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.title, "测试会话");
    assert_eq!(parsed.messages.len(), 2);
    assert_eq!(parsed.messages[0].text_content(), "你好");
}

#[test]
fn empty_object_is_rejected_required_fields() {
    // 回归：id/role/content/created_at 为必填字段。空对象 `{}` 必须反序列化失败，
    // 而非静默生成空编号、空正文的用户消息（与 origin/main 的 derive 行为一致）。
    let result = serde_json::from_str::<Message>("{}");
    assert!(
        result.is_err(),
        "空对象应因缺少必填字段而失败，实际得到：{:?}",
        result.ok()
    );
}

#[test]
fn missing_created_at_is_rejected() {
    // 缺少 created_at（必填）应失败
    let json = r#"{"id":"x","role":"user","content":"hi"}"#;
    let result = serde_json::from_str::<Message>(json);
    assert!(
        result.is_err(),
        "缺少 created_at 应失败，实际得到：{:?}",
        result.ok()
    );
}
