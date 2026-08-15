//! Core 端到端集成测试：真实 `TiangongCore` 实例 + wiremock 假 LLM，
//! 全部交互从 `deliver()` 发起，断言公开可见结果（事件流 + 磁盘终态）。
//!
//! 每个场景同时验证外部结果（终态/事件/落盘）与关键请求内容（结构化
//! 解析 mock 收到的请求正文），确保重构回归时测试必然失败。

use std::sync::Arc;
use std::time::Duration;

use tiangong_types::StreamEvent;
use tiangong_types::TurnStatus;
use wiremock::MockServer;

use super::test_support::*;
use crate::agent_input::{AgentInput, AgentInputKind};
use crate::permission::TrustMode;
use crate::session::MessageRole;

/// 场景 1：普通问答——请求含用户问题，回复落盘，恰好一个成功终态。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_question_completes_with_done_event() {
    let (env, sid) = TestEnv::new("plain");
    let server = MockServer::start().await;
    mount_sse(&server, vec![text_delta_chunk("贪心算法是一种……")], None).await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-1", "解释一下贪心算法");
    let status = wait_turn_status(&env, &sid, "msg-1").await;

    // 外部结果。
    assert_eq!(status, TurnStatus::Success);
    events.wait_done();
    events.assert_single_success_terminal();
    let session = env.load_session(&sid);
    assert!(
        session
            .messages
            .iter()
            .any(|m| m.text_content().contains("贪心算法是一种")),
        "最终回复应保存进 session"
    );
    // 关键请求内容：第一轮请求包含用户问题。
    let request = chat_request_at(&server, 0).await;
    assert!(
        request.role_message_contains("user", "解释一下贪心算法"),
        "首轮请求必须包含用户问题"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 2：多轮工具——首轮请求声明工具并允许调用，工具真实执行一次，
/// 第二轮请求包含模型工具调用与按编号对应的结果。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_roundtrip_executes_plugin_and_answers() {
    let (env, sid) = TestEnv::new("tool");
    let server = MockServer::start().await;
    let tool = RecordingTool::succeed("echo");
    let plugin = Arc::new(ToolPlugin {
        id: "echo-plugin",
        tool: tool.clone(),
    });
    let (core, mut events) = core_with(
        &env,
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
    let status = wait_turn_status(&env, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Success);

    // 外部结果。
    assert_eq!(tool.count(), 1, "工具应恰好执行一次");
    events.wait_done();
    events.assert_single_success_terminal();

    // 首轮请求：声明 echo 工具且允许调用。
    let first = chat_request_at(&server, 0).await;
    assert!(
        first.defined_tools().iter().any(|n| n == "echo"),
        "首轮请求必须包含 echo 工具定义"
    );
    assert!(first.allows_tool_calls(), "首轮请求必须允许模型调用工具");
    // 第二轮请求：模型工具调用与工具结果按编号一一对应。
    let second = chat_request_at(&server, 1).await;
    let declared = second.assistant_tool_calls();
    assert_eq!(
        declared,
        vec![("call-1".to_string(), "echo".to_string())],
        "第二轮请求应包含模型声明的工具调用（编号与名称）"
    );
    let results = second.tool_results();
    assert_eq!(results.len(), 1, "第二轮请求应包含恰好一个工具结果");
    assert_eq!(results[0].0, "call-1", "工具结果编号与声明调用一致");
    assert!(
        results[0].1.contains("done"),
        "工具结果内容与真实执行输出一致"
    );
    assert_eq!(
        tool.call_ids(),
        vec!["call-1".to_string()],
        "真实执行的调用编号与模型声明一致"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 3：审批批准——审批前工具零执行，批准后恰好执行一次，
/// 第二轮请求携带工具结果。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_granted_executes_tool_and_completes() {
    let (env, sid) = TestEnv::new("approve");
    let server = MockServer::start().await;
    let tool = RecordingTool::succeed("echo");
    let plugin = Arc::new(ToolPlugin {
        id: "echo-plugin",
        tool: tool.clone(),
    });
    let (core, mut events) = core_with(
        &env,
        &sid,
        &server.uri(),
        TrustMode::Supervised,
        vec![plugin],
    );

    mount_sse(&server, vec![tool_call_chunk("call-1", "echo", "{}")], None).await;
    mount_sse(&server, vec![text_delta_chunk("已按批准执行。")], None).await;

    send_message(&core, "msg-1", "执行 echo 工具");
    let request_id = events.wait_approval_needed();
    // 审批前工具不得执行。
    assert_eq!(tool.count(), 0, "审批前工具不得执行");

    core.deliver(AgentInputKind::approval(request_id, true))
        .expect("批准投递应成功");
    let status = wait_turn_status(&env, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Success);
    assert_eq!(tool.count(), 1, "批准后工具应恰好执行一次");
    events.wait_done();
    events.assert_single_success_terminal();

    // 第二轮请求包含工具结果（编号对应）。
    let second = chat_request_at(&server, 1).await;
    let results = second.tool_results();
    assert!(
        results
            .iter()
            .any(|(id, text)| id == "call-1" && text.contains("done")),
        "批准后的请求应包含与调用编号对应的工具结果"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 4：审批拒绝——工具始终不执行，第二轮请求包含与原调用编号
/// 对应的拒绝结果，最终解释产生于拒绝之后。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_rejected_records_result_and_model_explains() {
    let (env, sid) = TestEnv::new("reject");
    let server = MockServer::start().await;
    let tool = RecordingTool::succeed("echo");
    let plugin = Arc::new(ToolPlugin {
        id: "echo-plugin",
        tool: tool.clone(),
    });
    let (core, mut events) = core_with(
        &env,
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
    let request_id = events.wait_approval_needed();
    core.deliver(AgentInputKind::approval(request_id, false))
        .expect("拒绝投递应成功");

    let status = wait_turn_status(&env, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Success);
    assert_eq!(tool.count(), 0, "拒绝后工具始终不得执行");
    events.wait_done();
    events.assert_single_success_terminal();

    // 第二轮请求：与原调用编号对应的拒绝结果（结构化断言）。
    let second = chat_request_at(&server, 1).await;
    let results = second.tool_results();
    assert!(
        results
            .iter()
            .any(|(id, text)| id == "call-1" && text.contains("拒绝")),
        "拒绝结果必须与原调用编号配对出现在第二轮请求中"
    );
    // 最终解释在拒绝结果之后产生（会话消息顺序）。
    let session = env.load_session(&sid);
    let reject_pos = session
        .messages
        .iter()
        .position(|m| m.role == MessageRole::Tool && m.tool_call_id.as_deref() == Some("call-1"))
        .expect("会话中应存在拒绝工具结果");
    let explain_pos = session
        .messages
        .iter()
        .position(|m| m.text_content().contains("已按你的要求取消执行"))
        .expect("会话中应存在最终解释");
    assert!(explain_pos > reject_pos, "最终解释必须产生于拒绝结果之后");
    core.shutdown_join().expect("关闭失败");
}

/// 场景 5：运行中引导——第一轮请求确认发出并等待中，引导后出现第二条
/// 请求且包含新意图；新消息成为终态归属；首轮部分输出不作为最终答复。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_message_aborts_and_restarts_current_turn() {
    let (env, sid) = TestEnv::new("steer");
    let server = MockServer::start().await;
    mount_sse(
        &server,
        vec![text_delta_chunk("长时间任务执行中")],
        Some(Duration::from_secs(30)),
    )
    .await;
    mount_sse(&server, vec![text_delta_chunk("按新方向完成。")], None).await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-a", "开始一个长任务");
    // 确认第一条请求已发出（处于等待状态）。
    wait_requests(&server, 1).await;

    send_message(&core, "msg-steer", "换个方向处理");
    let status = wait_turn_status(&env, &sid, "msg-steer").await;
    assert_eq!(status, TurnStatus::Success, "新消息必须成为终态归属");
    events.wait_done();
    events.assert_single_success_terminal();

    // 引导后必须出现第二条请求且包含新意图。
    wait_requests(&server, 2).await;
    let second = chat_request_at(&server, 1).await;
    assert!(
        second.role_message_contains("user", "换个方向处理"),
        "重启后的请求必须包含新意图"
    );
    // 首轮的部分输出不得作为最终答复（Summary 相位）保存。
    let session = env.load_session(&sid);
    let final_replies = session
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant && m.text_content().contains("按新方向完成"))
        .count();
    assert_eq!(final_replies, 1, "最终答复只应有一条（新意图的结果）");
    assert!(
        !session.messages.iter().any(|m| {
            m.role == MessageRole::Assistant
                && m.text_content().trim() == "长时间任务执行中"
                && m.phase == crate::session::MessagePhase::Summary
        }),
        "被引导中断的部分输出不得作为最终答复保存"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 6：取消——取消发生在请求运行中，磁盘为取消终态，只发布一个
/// 取消终态，取消后不再发起新请求或产生成功事件。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_running_turn_ends_cancelled() {
    let (env, sid) = TestEnv::new("cancel");
    let server = MockServer::start().await;
    mount_sse(
        &server,
        vec![text_delta_chunk("长时间任务执行中")],
        Some(Duration::from_secs(30)),
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-1", "开始一个长任务");
    wait_requests(&server, 1).await; // 取消发生在请求运行中。

    core.deliver(AgentInputKind::cancel())
        .expect("取消投递应成功");
    let status = wait_turn_status(&env, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Cancelled);
    events.assert_single_cancelled_terminal();

    // 取消后不得再发起新请求。
    let before = server.received_requests().await.unwrap().len();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = server.received_requests().await.unwrap().len();
    assert_eq!(before, after, "取消后不得继续发起模型请求");
    core.shutdown_join().expect("关闭失败");
}

/// 场景 7：空闲注入——注入后零请求；下一自然轮请求把注入表示为
/// 结构化工具结果（编号与内容），而非任意字符串。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_injection_deferred_into_next_request() {
    let (env, sid) = TestEnv::new("inject");
    let server = MockServer::start().await;
    mount_sse(
        &server,
        vec![text_delta_chunk("已结合页面信息回答。")],
        None,
    )
    .await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    core.deliver(AgentInputKind::tool(
        "browser_observation",
        serde_json::json!({"summary": "页面加载完成-INJECT-MARK", "url": "https://example.com"}),
    ))
    .expect("空闲注入应被接受");
    assert!(
        !crate::react::inbox::is_running(&sid),
        "inject 不得启动轮次"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "注入后不得发出任何模型请求"
    );

    send_message(&core, "msg-1", "根据页面情况回答");
    let status = wait_turn_status(&env, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Success);
    events.wait_done();
    events.assert_single_success_terminal();

    // 下一自然轮请求：注入表示为 plugin_injection 的工具结果（与 assistant
    // 声明的调用配对），数据来源 browser_observation 与内容均在结果文本中。
    let request = chat_request_at(&server, 0).await;
    let declared = request.assistant_tool_calls();
    let results = request.tool_results();
    assert!(
        results.iter().any(|(id, text)| {
            text.contains("browser_observation")
                && text.contains("INJECT-MARK")
                && declared.iter().any(|(decl_id, _)| decl_id == id)
        }),
        "注入必须是配对的工具调用/结果（来源 browser_observation + 内容标记）"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 场景 8：自动压缩——压缩请求折叠旧历史且不含保留的最近交互、不带
/// 工具定义；压缩后请求含摘要与当前问题；摘要与边界落盘。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn high_pressure_triggers_pre_request_compression() {
    let (env, sid) = TestEnv::new("compress");
    let server = MockServer::start().await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

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
        wait_turn_status(&env, &sid, "msg-1").await,
        TurnStatus::Success
    );

    // 第二轮：请求前压缩，随后模型请求完成。
    mount_completion(&server, "[[SUMMARY]]\n压缩后的历史摘要", None).await;
    mount_sse(&server, vec![text_delta_chunk("结合摘要回答。")], None).await;
    send_message(&core, "msg-2", "第二个问题");
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-2").await,
        TurnStatus::Success
    );
    events.wait_done();
    events.assert_done_count(2); // 两个 turn 各一次成功终态。

    // 压缩请求（#1）：含旧历史、不含保留的最近交互、不带工具定义。
    let compression = chat_request_at(&server, 1).await;
    assert!(
        compression.any_message_contains("第一个问题"),
        "压缩请求应包含被折叠的旧历史"
    );
    assert!(
        !compression.any_message_contains("第二个问题"),
        "压缩请求不得包含必须保留的最近交互"
    );
    assert!(
        compression.defined_tools().is_empty(),
        "压缩请求不得携带工具定义"
    );
    // 压缩后的模型请求（#2）：含摘要与当前问题。
    let post = chat_request_at(&server, 2).await;
    assert!(
        post.any_message_contains("压缩后的历史摘要"),
        "压缩后请求应包含摘要"
    );
    assert!(
        post.role_message_contains("user", "第二个问题"),
        "压缩后请求应包含当前问题"
    );
    // 摘要与边界落盘。
    let session = env.load_session(&sid);
    assert_eq!(session.context_summary.as_deref(), Some("压缩后的历史摘要"));
    assert!(session.summary_up_to > 0, "压缩边界应推进");
    core.shutdown_join().expect("关闭失败");
}

/// 场景 9：手动压缩——压缩请求只含被折叠历史；成功动作（Compress）
/// 事件；摘要、边界与事件一致；结束后 driver 回到空闲。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compression_applies_summary() {
    let (env, sid) = TestEnv::new("manual-compress");
    let server = MockServer::start().await;
    mount_completion(&server, "[[SUMMARY]]\n手动压缩摘要内容", None).await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    // 预置可压缩历史。
    {
        let mut session = env.load_session(&sid);
        session.append_message(MessageRole::User, "第一条问题");
        session.append_message(MessageRole::User, "第二条问题");
        session.try_persist_to_disk().unwrap();
    }

    core.deliver(AgentInputKind::compress_context())
        .expect("手动压缩应被接受");
    // 成功动作事件（不是任意压缩结束事件）。
    events.wait_context_compressed(tiangong_types::stream::ContextCompressAction::Compress);

    // 压缩请求的历史范围：含被折叠历史。
    let compression = chat_request_at(&server, 0).await;
    assert!(
        compression.any_message_contains("第一条问题"),
        "压缩请求应包含被折叠的历史"
    );
    // 摘要、边界与事件内容一致。
    let session = env.load_session(&sid);
    assert_eq!(
        session.context_summary.as_deref(),
        Some("手动压缩摘要内容"),
        "摘要应落盘"
    );
    assert!(session.summary_up_to > 0, "压缩边界应推进");
    // 压缩结束后执行循环回到空闲。
    wait_idle(&sid).await;
    core.shutdown_join().expect("关闭失败");
}

/// 场景 10：模型失败——重试符合配置（同一请求重发一次），最终磁盘
/// 失败，恰好一个失败终态，无成功事件。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn llm_failure_propagates_failed_status() {
    let (env, sid) = TestEnv::new("llm-fail");
    let server = MockServer::start().await;
    mount_permanent_error(&server).await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-1", "随便问点");
    let status = wait_turn_status(&env, &sid, "msg-1").await;
    assert_eq!(status, TurnStatus::Failed);

    // 模型客户端对 400 有一次内部重试：同一请求共发出两次。
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        2,
        "失败应经过客户端的一次内部重试（共两次请求）"
    );
    events.assert_single_failure_terminal("失败");
    core.shutdown_join().expect("关闭失败");
}

// ── 边界场景 ──────────────────────────────────────────────────────────

/// 边界 1：turn 结束交接——A 完成后立即投递 B：B 不丢失、不并入 A，
/// 同一 driver 自动开始下一轮，B 的请求从 A 完成后的最新会话构建。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_turn_hands_off_next_message() {
    let (env, sid) = TestEnv::new("handoff");
    let server = MockServer::start().await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    // A：文本完成（含唯一标记）。
    let marker = format!("A-FINAL-{sid}");
    mount_sse(&server, vec![text_delta_chunk(&marker)], None).await;
    // B：文本完成。
    mount_sse(&server, vec![text_delta_chunk("B 的回答。")], None).await;

    send_message(&core, "msg-a", "第一个问题");
    // A 完成落盘后（交接窗口起点），立即投递 B。
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-a").await,
        TurnStatus::Success
    );
    send_message(&core, "msg-b", "第二个问题");

    // B 不丢失且由下一轮处理（同一 driver 自动开始）。
    let status = wait_turn_status(&env, &sid, "msg-b").await;
    assert_eq!(status, TurnStatus::Success, "B 必须被执行并获得终态");
    events.wait_done();
    events.pump();
    let done_count = events
        .seen()
        .iter()
        .filter(|e| matches!(e, StreamEvent::Done { .. }))
        .count();
    assert_eq!(done_count, 2, "A 与 B 各发布一次成功终态");

    // B 的请求从 A 完成后的最新会话构建（含 A 的最终回复）。
    let second = chat_request_at(&server, 1).await;
    assert!(
        second.any_message_contains(&marker),
        "B 的请求必须包含 A 的最终回复（最新会话）"
    );
    // B 未被并入 A：B 的消息独立存在。
    let session = env.load_session(&sid);
    assert!(session.messages.iter().any(|m| m.id == "msg-b"));
    core.shutdown_join().expect("关闭失败");
}

/// 边界 2：单槽占用忙碌拒绝——无可引导活动轮的间隙投递 B 占用待执行
/// 槽，C 必须明确 Busy；B 落盘执行，C 不得悄悄出现。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn occupied_pending_slot_returns_busy() {
    let (env, sid) = TestEnv::new("busy");
    let server = MockServer::start().await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    mount_sse(
        &server,
        vec![text_delta_chunk("挂起中")],
        Some(Duration::from_secs(30)),
    )
    .await;
    mount_sse(&server, vec![text_delta_chunk("B 完成。")], None).await;

    send_message(&core, "msg-a", "第一个问题");
    wait_requests(&server, 1).await;
    // 取消 A（进入结束路径），等待 driver 回到无可引导活动的间隙。
    core.deliver(AgentInputKind::cancel()).unwrap();
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-a").await,
        TurnStatus::Cancelled
    );

    // B 占用待执行槽。
    send_message(&core, "msg-b", "第二个问题");
    // C 必须明确忙碌。
    let c_result = core.deliver(AgentInputKind::prepared_with_id(
        "msg-c",
        vec![tiangong_types::ContentBlock::text("第三个问题")],
    ));
    assert!(
        matches!(c_result, Err(crate::core::CoreError::Busy)),
        "单槽占用时必须明确 Busy，当前: {c_result:?}"
    );

    // B 落盘执行；C 不得悄悄出现。
    let status = wait_turn_status(&env, &sid, "msg-b").await;
    assert_eq!(status, TurnStatus::Success);
    events.wait_done();
    let session = env.load_session(&sid);
    assert!(
        !session.messages.iter().any(|m| m.id == "msg-c"),
        "被拒绝的 C 不得出现在会话中"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 边界 3：已接受消息必须在关闭后存活——A 投递并被接收保存后立即关闭，
/// 无条件检查 A 的归宿（最终会话存在或关闭明确失败）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_messages_survive_shutdown() {
    let (env, sid) = TestEnv::new("survive");
    let server = MockServer::start().await;
    mount_sse(
        &server,
        vec![text_delta_chunk("长时间任务执行中")],
        Some(Duration::from_secs(30)),
    )
    .await;
    let (core, _events) = core_for(&env, &sid, &server.uri());

    // A：投递并等待其被保存进会话（已确认接收）。
    send_message(&core, "msg-a", "关闭前正在处理的消息");
    {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            let saved = env
                .load_session(&sid)
                .messages
                .iter()
                .any(|m| m.id == "msg-a");
            if saved {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "等待 A 保存超时");
            tokio::time::sleep(POLL).await;
        }
    }

    // 立即关闭。
    let shutdown = core.shutdown_join();
    // 无条件检查 A：存在于最终会话，或关闭明确失败。
    let a_in_final = env
        .load_session(&sid)
        .messages
        .iter()
        .any(|m| m.id == "msg-a");
    assert!(
        a_in_final || shutdown.is_err(),
        "已接受并保存的 A 必须在最终会话存活，或关闭明确失败"
    );
}

/// 边界 4：候选完成窗口注入——模型产生可结束文本后立即注入工具结果。
/// 注入落点有两种合法时序，测试按实际落点分支断言（不依赖竞态碰撞）：
/// - 落在候选完成窗口内：提交被撤销，出现下一次模型请求且包含注入结果，
///   最终答复为结合注入后的结论；
/// - 落在提交之后：turn 正常成功，注入积压不丢失，随下一条消息消费。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inject_during_candidate_finish_reopens_loop() {
    let (env, sid) = TestEnv::new("candidate-inject");
    let server = MockServer::start().await;
    let (core, mut events) = core_for(&env, &sid, &server.uri());

    mount_sse(&server, vec![text_delta_chunk("初步结论如下。")], None).await;
    mount_sse(
        &server,
        vec![text_delta_chunk("结合注入信息的最终结论。")],
        None,
    )
    .await;
    // 落点 b（提交后积压）时，供下一条消息消费的请求。
    mount_sse(
        &server,
        vec![text_delta_chunk("下一轮结合注入回答。")],
        None,
    )
    .await;

    send_message(&core, "msg-1", "分析并等待补充信息");
    // 等待第一轮响应文本流完（进入候选完成窗口附近），立即注入工具结果。
    events.wait_until(
        |e| matches!(e, StreamEvent::ReactText { content, .. } if content.contains("初步结论")),
        "第一轮响应文本",
    );
    core.deliver(AgentInputKind::tool(
        "browser_observation",
        serde_json::json!({"summary": "CANDIDATE-INJECT-MARK", "url": "https://example.com"}),
    ))
    .expect("候选完成窗口的注入应被接受");

    // 确定性区分落点：msg-1 是否已获得终态（时间预算在高并发下不可靠）。
    // 已终态 = 落点 b（提交完成，注入积压）；无终态 = 落点 a（撤销进行中）。
    let msg1_settled = {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            let settled = env
                .load_session(&sid)
                .messages
                .iter()
                .any(|m| m.id == "msg-1" && m.turn_status.is_some());
            if settled {
                break true;
            }
            // 撤销路径出现第二次请求即确认落点 a。
            if server.received_requests().await.map_or(0, |r| r.len()) >= 2 {
                break false;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "等待落点确定（msg-1 终态或第二次请求）超时"
            );
            tokio::time::sleep(POLL).await;
        }
    };
    let reopened = !msg1_settled;
    if reopened {
        let status = wait_turn_status(&env, &sid, "msg-1").await;
        assert_eq!(status, TurnStatus::Success);
        let second = chat_request_at(&server, 1).await;
        assert!(
            second.tool_results().iter().any(|(_, text)| {
                text.contains("browser_observation") && text.contains("CANDIDATE-INJECT-MARK")
            }),
            "撤销后的请求必须包含注入的工具结果（来源与内容）"
        );
        events.wait_done();
        events.assert_single_success_terminal();
        let session = env.load_session(&sid);
        assert!(
            session
                .messages
                .iter()
                .any(|m| m.text_content().contains("结合注入信息的最终结论")),
            "最终答复应为结合注入后的结论"
        );
    } else {
        // 落点 b：turn 已正常成功，注入积压未丢失——下一条消息的请求
        // 必须包含注入内容。
        let status = wait_turn_status(&env, &sid, "msg-1").await;
        assert_eq!(status, TurnStatus::Success, "落点 b 时 turn 正常成功");
        send_message(&core, "msg-2", "继续");
        wait_requests(&server, 2).await;
        let status2 = wait_turn_status(&env, &sid, "msg-2").await;
        assert_eq!(status2, TurnStatus::Success);
        let second = chat_request_at(&server, 1).await;
        assert!(
            second.tool_results().iter().any(|(_, text)| {
                text.contains("browser_observation") && text.contains("CANDIDATE-INJECT-MARK")
            }),
            "积压的注入必须随下一条消息进入请求（不丢失）"
        );
        events.pump();
        assert_eq!(
            events
                .seen()
                .iter()
                .filter(|e| matches!(e, StreamEvent::Done { .. }))
                .count(),
            2,
            "两个 turn 各一次成功终态"
        );
    }
    core.shutdown_join().expect("关闭失败");
}
