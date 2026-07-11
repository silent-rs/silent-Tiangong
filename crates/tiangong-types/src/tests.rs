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
    let waiting = serde_json::to_string(&RunStatus::WaitingApproval).unwrap();
    assert_eq!(waiting, r#""waiting_approval""#);
    let parsed: RunStatus = serde_json::from_str(r#""idle""#).unwrap();
    assert_eq!(parsed, RunStatus::Idle);
    let parsed_waiting: RunStatus = serde_json::from_str(r#""waiting_approval""#).unwrap();
    assert_eq!(parsed_waiting, RunStatus::WaitingApproval);
    let parsed_legacy_waiting: RunStatus = serde_json::from_str(r#""waitingapproval""#).unwrap();
    assert_eq!(parsed_legacy_waiting, RunStatus::WaitingApproval);
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
