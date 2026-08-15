//! Core 端到端集成测试：真实 `TiangongCore` 实例 + wiremock 假 LLM，
//! 全部交互从 `deliver()` 发起，断言公开可见结果（事件流 + 磁盘终态）。
//!
//! 覆盖：普通问答、多轮工具、审批批准/拒绝、运行中引导、取消、
//! 空闲注入顺延、自动压缩、手动压缩、LLM 失败传播。

use std::sync::Arc;
use std::time::Duration;

use tiangong_types::StreamEvent;
use tiangong_types::TurnStatus;
use wiremock::MockServer;

use super::test_support::*;
use crate::agent_input::{AgentInput, AgentInputKind};
use crate::permission::TrustMode;

fn temp_root() -> std::path::PathBuf {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let path = dir.path().to_path_buf();
    std::mem::forget(dir);
    path
}

fn sid(tag: &str) -> String {
    format!("it-{tag}-{}", scru128::new_string())
}

/// 场景 1：普通问答——deliver 一条消息，mock 返回纯文本，
/// turn 以 Success 结束，最终回复落盘，事件流含 Done。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_question_completes_with_done_event() {
    let root = temp_root();
    let sid = sid("plain");
    let server = MockServer::start().await;
    mount_sse(&server, vec![text_delta_chunk("贪心算法是一种……")], None).await;
    let (core, event_rx) = core_for(&root, &sid, &server.uri());

    send_message(&core, "msg-1", "解释一下贪心算法");
    let status = wait_turn_status(&root, &sid, "msg-1").await;

    assert_eq!(status, TurnStatus::Success);
    let events = drain_events(&event_rx);
    assert_done(&events);
    let session = crate::session::Session::load_from_storage(&root, &sid).unwrap();
    assert!(
        session
            .messages
            .iter()
            .any(|m| m.text_content().contains("贪心算法是一种")),
        "最终回复应保存进 session"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 2：多轮工具——模型先调用工具（真实执行插件工具），
/// 拿到结果后作答，工具结果进入第二轮请求上下文。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_roundtrip_executes_plugin_and_answers() {
    let root = temp_root();
    let sid = sid("tool");
    let server = MockServer::start().await;
    let tool = RecordingTool::succeed("echo");
    let plugin = Arc::new(ToolPlugin {
        id: "echo-plugin",
        tool: tool.clone(),
    });
    let (core, event_rx) = core_with(
        &root,
        &sid,
        &server.uri(),
        TrustMode::FullTrust,
        vec![plugin],
    );

    mount_sse(&server, vec![tool_call_chunk("call-1", "echo", "{}")], None).await;
    mount_sse(
        &server,
        vec![text_delta_chunk("工具已执行，结果是 done。")],
        None,
    )
    .await;

    send_message(&core, "msg-1", "执行 echo 工具");
    let status = wait_turn_status(&root, &sid, "msg-1").await;

    assert_eq!(status, TurnStatus::Success);
    assert_eq!(tool.count(), 1, "插件工具应被真实执行一次");
    let events = drain_events(&event_rx);
    assert_done(&events);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolStart { name, .. } if name == "echo")),
        "事件流应包含工具开始事件"
    );
    // 第二轮请求应携带工具结果。
    let requests = server.received_requests().await.unwrap();
    let second = &requests[1];
    assert!(
        String::from_utf8_lossy(&second.body).contains("done"),
        "第二轮请求上下文应包含工具结果"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 3：审批批准——监督模式下工具等待审批，批准后执行并完成。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_granted_executes_tool_and_completes() {
    let root = temp_root();
    let sid = sid("approve");
    let server = MockServer::start().await;
    let tool = RecordingTool::succeed("echo");
    let plugin = Arc::new(ToolPlugin {
        id: "echo-plugin",
        tool: tool.clone(),
    });
    let (core, event_rx) = core_with(
        &root,
        &sid,
        &server.uri(),
        TrustMode::Supervised,
        vec![plugin],
    );

    mount_sse(&server, vec![tool_call_chunk("call-1", "echo", "{}")], None).await;
    mount_sse(&server, vec![text_delta_chunk("已按批准执行。")], None).await;

    send_message(&core, "msg-1", "执行 echo 工具");
    // 等待审批请求事件并批准。
    let core_holder = core;
    let approval_tx_core = &core_holder;
    let deadline = std::time::Instant::now() + WAIT;
    let mut request_id = None;
    loop {
        for event in drain_events(&event_rx) {
            if let StreamEvent::ApprovalNeeded {
                request_id: rid, ..
            } = event
            {
                request_id = Some(rid);
            }
        }
        if request_id.is_some() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "等待审批事件超时");
        tokio::time::sleep(POLL).await;
    }
    approval_tx_core
        .deliver(AgentInputKind::approval(request_id.unwrap(), true))
        .expect("批准投递应成功");

    let status = wait_turn_status(&root, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Success);
    assert_eq!(tool.count(), 1, "批准后工具应执行");
    assert_done(&drain_events(&event_rx));
    core_holder.shutdown_join().expect("关闭失败");
}

/// 场景 4：审批拒绝——拒绝结果写入会话，模型看到后解释结束；
/// 工具未执行。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_rejected_records_result_and_model_explains() {
    let root = temp_root();
    let sid = sid("reject");
    let server = MockServer::start().await;
    let tool = RecordingTool::succeed("echo");
    let plugin = Arc::new(ToolPlugin {
        id: "echo-plugin",
        tool: tool.clone(),
    });
    let (core, event_rx) = core_with(
        &root,
        &sid,
        &server.uri(),
        TrustMode::Supervised,
        vec![plugin],
    );

    mount_sse(&server, vec![tool_call_chunk("call-1", "echo", "{}")], None).await;
    mount_sse(
        &server,
        vec![text_delta_chunk("好的，已按你的要求取消执行。")],
        None,
    )
    .await;

    send_message(&core, "msg-1", "执行 echo 工具");
    let deadline = std::time::Instant::now() + WAIT;
    let mut request_id = None;
    loop {
        for event in drain_events(&event_rx) {
            if let StreamEvent::ApprovalNeeded {
                request_id: rid, ..
            } = event
            {
                request_id = Some(rid);
            }
        }
        if request_id.is_some() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "等待审批事件超时");
        tokio::time::sleep(POLL).await;
    }
    core.deliver(AgentInputKind::approval(request_id.unwrap(), false))
        .expect("拒绝投递应成功");

    let status = wait_turn_status(&root, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Success);
    assert_eq!(tool.count(), 0, "拒绝后工具不得执行");
    let session = crate::session::Session::load_from_storage(&root, &sid).unwrap();
    assert!(
        session
            .messages
            .iter()
            .any(|m| m.text_content().contains("拒绝")),
        "拒绝结果应写入会话供模型看到"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 5：运行中引导——turn A 挂起时投递新消息，当前活动中止、
/// 新消息保存、从新意图重启并完成。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_message_aborts_and_restarts_current_turn() {
    let root = temp_root();
    let sid = sid("steer");
    let server = MockServer::start().await;
    // A 挂起 30s；引导重启后的请求完成本轮。
    mount_sse(
        &server,
        vec![text_delta_chunk("长时间任务执行中")],
        Some(Duration::from_secs(30)),
    )
    .await;
    mount_sse(&server, vec![text_delta_chunk("按新方向完成。")], None).await;
    let (core, _event_rx) = core_for(&root, &sid, &server.uri());

    send_message(&core, "msg-a", "开始一个长任务");
    wait_requests(&server, 1).await;

    send_message(&core, "msg-steer", "换个方向处理");
    let status = wait_turn_status(&root, &sid, "msg-steer").await;
    assert_eq!(status, TurnStatus::Success, "引导消息应驱动完成");

    let session = crate::session::Session::load_from_storage(&root, &sid).unwrap();
    assert!(
        session.messages.iter().any(|m| m.id == "msg-steer"),
        "引导消息应保存进 session"
    );
    // 引导后 turn 有最终回复。
    assert!(
        session
            .messages
            .iter()
            .any(|m| m.text_content().contains("按新方向完成")),
        "重启后应产出最终回复"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 6：取消——运行中投递 Cancel，turn 以 Cancelled 终态结束。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_running_turn_ends_cancelled() {
    let root = temp_root();
    let sid = sid("cancel");
    let server = MockServer::start().await;
    mount_sse(
        &server,
        vec![text_delta_chunk("长时间任务执行中")],
        Some(Duration::from_secs(30)),
    )
    .await;
    let (core, event_rx) = core_for(&root, &sid, &server.uri());

    send_message(&core, "msg-1", "开始一个长任务");
    wait_requests(&server, 1).await;

    core.deliver(AgentInputKind::cancel())
        .expect("取消投递应成功");
    let status = wait_turn_status(&root, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Cancelled);
    let events = drain_events(&event_rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error { message } if message.contains("取消"))),
        "取消终态应经事件流发布"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 7：空闲注入顺延——空闲期工具注入被接受但不启动轮次；
/// 下一条消息的模型请求上下文包含注入内容。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_injection_deferred_into_next_request() {
    let root = temp_root();
    let sid = sid("inject");
    let server = MockServer::start().await;
    mount_sse(
        &server,
        vec![text_delta_chunk("已结合页面信息回答。")],
        None,
    )
    .await;
    let (core, _event_rx) = core_for(&root, &sid, &server.uri());

    core.deliver(AgentInputKind::tool(
        "browser_observation",
        serde_json::json!({"summary": "页面加载完成-INJECT-MARK", "url": "https://example.com"}),
    ))
    .expect("空闲注入应被接受");
    assert!(
        !crate::react::inbox::is_running(&sid),
        "inject 不得启动轮次"
    );

    send_message(&core, "msg-1", "根据页面情况回答");
    let status = wait_turn_status(&root, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Success);

    let requests = server.received_requests().await.unwrap();
    assert!(
        String::from_utf8_lossy(&requests[0].body).contains("INJECT-MARK"),
        "注入内容应出现在下一轮请求上下文中"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 8：自动压缩——上一轮观测压力超阈值（流式 usage 传递），下一轮
/// 请求前先压缩，摘要进入压缩后请求的 system 上下文。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn high_pressure_triggers_pre_request_compression() {
    let root = temp_root();
    let sid = sid("compress");
    let server = MockServer::start().await;
    let (core, _event_rx) = core_for(&root, &sid, &server.uri());

    // 第一轮：大用量文本完成（建立压力信号）。
    mount_sse(
        &server,
        vec![
            text_delta_chunk("第一轮完成。"),
            usage_delta_chunk(185_900, 5),
        ],
        None,
    )
    .await;
    send_message(&core, "msg-1", "第一个问题");
    assert_eq!(
        wait_turn_status(&root, &sid, "msg-1").await,
        TurnStatus::Success
    );

    // 第二轮：请求前压缩，随后模型请求完成。
    mount_completion(&server, "[[SUMMARY]]\n压缩后的历史摘要", None).await;
    mount_sse(&server, vec![text_delta_chunk("结合摘要回答。")], None).await;
    send_message(&core, "msg-2", "第二个问题");
    assert_eq!(
        wait_turn_status(&root, &sid, "msg-2").await,
        TurnStatus::Success
    );

    let requests = server.received_requests().await.unwrap();
    // 请求序：#0 第一轮模型；#1 压缩；#2 压缩后模型。
    assert_eq!(requests.len(), 3, "应发出 模型→压缩→模型 三个请求");
    let post_compress = String::from_utf8_lossy(&requests[2].body);
    assert!(
        post_compress.contains("压缩后的历史摘要"),
        "压缩摘要应进入压缩后请求的上下文"
    );
    let session = crate::session::Session::load_from_storage(&root, &sid).unwrap();
    assert_eq!(
        session.context_summary.as_deref(),
        Some("压缩后的历史摘要"),
        "摘要应落盘"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 9：手动压缩——空闲期触发，压缩请求发出且摘要落盘。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compression_applies_summary() {
    let root = temp_root();
    let sid = sid("manual-compress");
    let server = MockServer::start().await;
    mount_completion(&server, "[[SUMMARY]]\n手动压缩摘要内容", None).await;
    let (core, event_rx) = core_for(&root, &sid, &server.uri());

    // 预置可压缩历史。
    {
        let mut session = crate::session::Session::load_from_storage(&root, &sid).unwrap();
        session.append_message(crate::session::MessageRole::User, "第一条问题");
        session.append_message(crate::session::MessageRole::User, "第二条问题");
        session.try_persist_to_disk().unwrap();
    }

    core.deliver(AgentInputKind::compress_context())
        .expect("手动压缩应被接受");
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let applied = crate::session::Session::load_from_storage(&root, &sid)
            .map(|s| s.context_summary.is_some())
            .unwrap_or(false);
        if applied {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "等待手动压缩应用超时");
        tokio::time::sleep(POLL).await;
    }
    let events = drain_events(&event_rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ContextCompressed { .. })),
        "压缩完成应发布事件"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 10：LLM 失败——模型请求持续失败，turn 以 Failed 终态结束。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn llm_failure_propagates_failed_status() {
    let root = temp_root();
    let sid = sid("llm-fail");
    let server = MockServer::start().await;
    mount_permanent_error(&server).await;
    let (core, event_rx) = core_for(&root, &sid, &server.uri());

    send_message(&core, "msg-1", "随便问点");
    let status = wait_turn_status(&root, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Failed);
    let events = drain_events(&event_rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error { .. })),
        "失败终态应经事件流发布"
    );
    core.shutdown_join().expect("关闭失败");
}
