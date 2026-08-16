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
async fn plain_question_completes_with_done_event() {
    let (env, sid) = TestEnv::new("plain");
    let server = MockServer::start().await;
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
async fn tool_roundtrip_executes_plugin_and_answers() {
    let (env, sid) = TestEnv::new("tool");
    let server = MockServer::start().await;
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
async fn approval_granted_executes_tool_and_completes() {
    let (env, sid) = TestEnv::new("approve");
    let server = MockServer::start().await;
    let tool = RecordingTool::succeed("echo");
    let plugin = Arc::new(ToolPlugin {
        id: "echo-plugin",
        tool: tool.clone(),
    });
    let routes = mount_prompt_router(
        &server,
        vec![
            PromptRoute::new(
                "approved-result",
                |request| request.has_tool_result("approve-call", "done"),
                MockReply::sse(stream_text_chunks(&["已按批准", "执行。"])),
            ),
            PromptRoute::new(
                "approval-call",
                |request| {
                    request
                        .latest_user_text()
                        .is_some_and(|text| text.contains("审批后执行 echo"))
                        && request.tool_results().is_empty()
                },
                MockReply::sse(stream_tool_call_chunks("approve-call", "echo", &["{", "}"])),
            ),
        ],
    )
    .await;
    let (core, mut events) = core_with(
        &env,
        &sid,
        &server.uri(),
        TrustMode::Supervised,
        vec![plugin],
    );

    send_message(&core, "msg-1", "审批后执行 echo");
    let request_id = events.wait_approval_needed();
    assert_eq!(tool.count(), 0, "审批前工具不得执行");
    core.deliver(AgentInputKind::approval(request_id, true))
        .expect("批准投递应成功");

    assert_eq!(
        wait_turn_status(&env, &sid, "msg-1").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    assert_eq!(tool.count(), 1, "批准后工具应恰好执行一次");
    events.wait_done();
    events.assert_single_success_terminal();
    assert!(
        chat_request_at(&server, 1)
            .await
            .has_tool_result("approve-call", "done")
    );
    routes["approval-call"].assert_hits(1);
    routes["approved-result"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_rejected_records_result_and_model_explains() {
    let (env, sid) = TestEnv::new("reject");
    let server = MockServer::start().await;
    let tool = RecordingTool::succeed("echo");
    let plugin = Arc::new(ToolPlugin {
        id: "echo-plugin",
        tool: tool.clone(),
    });
    let routes = mount_prompt_router(
        &server,
        vec![
            PromptRoute::new(
                "rejected-result",
                |request| request.has_tool_result("reject-call", "拒绝"),
                MockReply::sse(stream_text_chunks(&["好的，", "已取消执行。"])),
            ),
            PromptRoute::new(
                "rejection-call",
                |request| {
                    request
                        .latest_user_text()
                        .is_some_and(|text| text.contains("拒绝执行 echo"))
                        && request.tool_results().is_empty()
                },
                MockReply::sse(stream_tool_call_chunks("reject-call", "echo", &["{", "}"])),
            ),
        ],
    )
    .await;
    let (core, mut events) = core_with(
        &env,
        &sid,
        &server.uri(),
        TrustMode::Supervised,
        vec![plugin],
    );

    send_message(&core, "msg-1", "拒绝执行 echo");
    let request_id = events.wait_approval_needed();
    core.deliver(AgentInputKind::approval(request_id, false))
        .expect("拒绝投递应成功");

    assert_eq!(
        wait_turn_status(&env, &sid, "msg-1").await,
        TurnStatus::Success
    );
    wait_idle(&sid).await;
    assert_eq!(tool.count(), 0, "拒绝后工具不得执行");
    events.wait_done();
    events.assert_single_success_terminal();

    let second = chat_request_at(&server, 1).await;
    assert!(second.has_tool_result("reject-call", "拒绝"));
    let session = env.load_session(&sid);
    let reject_pos = session
        .messages
        .iter()
        .position(|message| {
            message.role == MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("reject-call")
        })
        .expect("会话中应存在拒绝工具结果");
    let explain_pos = session
        .messages
        .iter()
        .position(|message| message.text_content().contains("已取消执行"))
        .expect("会话中应存在最终解释");
    assert!(explain_pos > reject_pos);
    routes["rejection-call"].assert_hits(1);
    routes["rejected-result"].assert_hits(1);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_message_aborts_and_restarts_current_turn() {
    let (env, sid) = TestEnv::new("steer");
    let server = MockServer::start().await;
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
    let server = MockServer::start().await;
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
    let server = MockServer::start().await;
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
    let server = MockServer::start().await;
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
    assert!(!compression.any_message_contains("AUTO-COMPRESS-SECOND"));
    assert!(compression.defined_tools().is_empty());
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
    let server = MockServer::start().await;
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
    let server = MockServer::start().await;
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
    assert_eq!(requests.len(), 2, "流式失败后应出现一次非流式回退");
    assert!(chat_request_at(&server, 0).await.is_stream());
    assert!(!chat_request_at(&server, 1).await.is_stream());
    routes["forced-failure"].assert_hits(2);
    core.shutdown_join().expect("关闭失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_turn_reads_latest_session() {
    let (env, sid) = TestEnv::new("next-latest");
    let server = MockServer::start().await;
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
    let server = MockServer::start().await;
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
    let server = MockServer::start().await;
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
    let server = MockServer::start().await;
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
    let server = MockServer::start().await;
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
