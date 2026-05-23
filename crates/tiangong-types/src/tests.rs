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
    };
    let b = TokenUsage {
        prompt_tokens: 200,
        completion_tokens: 100,
        total_tokens: 300,
    };
    a.accumulate(&b);
    assert_eq!(a.total_tokens, 450);
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
fn message_role_serde() {
    let json = serde_json::to_string(&MessageRole::Assistant).unwrap();
    assert_eq!(json, r#""assistant""#);
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
