//! Inbox、关闭与单执行驱动的内部可靠性契约测试。
//!
//! 外部交互场景见 `integration_tests.rs`；mock 与实例构建辅助复用
//! `test_support.rs`。

use std::time::{Duration, Instant};

use crate::agent_input::{AgentInput, AgentInputKind};
use crate::core::TiangongCore;

use super::test_support::{
    POLL, TestEnv, WAIT, mount_completion, mount_permanent_error, mount_sse, send_message,
    text_delta_chunk, wait_turn_status,
};

fn core_for(env: &TestEnv, sid: &str, endpoint: &str) -> TiangongCore {
    let (core, _events) = super::test_support::core_for(env, sid, endpoint);
    core
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_next_turn_reads_latest_session() {
    let (env, sid) = TestEnv::new("latest-session");
    let server = wiremock::MockServer::start().await;
    let core = core_for(&env, &sid, &server.uri());

    // turn A 直接文本完成（带唯一标记），随后 B 的请求快速失败（可捕获 body）。
    let marker = format!("SEALED-TURN-A-FINAL-{sid}");
    mount_sse(&server, vec![text_delta_chunk(&marker)], None).await;
    mount_permanent_error(&server).await;

    core.deliver(AgentInputKind::prepared_with_id(
        format!("{sid}-a"),
        vec![tiangong_types::ContentBlock::text("第一条消息")],
    ))
    .expect("A 应被接受");
    let a_id = format!("{sid}-a");
    let deadline = Instant::now() + WAIT;
    loop {
        let settled = env
            .load_session(&sid)
            .messages
            .iter()
            .any(|m| m.id == a_id && m.turn_status.is_some());
        if settled {
            break;
        }
        assert!(Instant::now() < deadline, "等待 turn A 完成超时");
        tokio::time::sleep(POLL).await;
    }

    // A 完成后投递 B：直接执行，请求必须包含 A 的最终回复。
    core.deliver(AgentInputKind::prepared_with_id(
        format!("{sid}-b"),
        vec![tiangong_types::ContentBlock::text("请基于上一轮结果继续")],
    ))
    .expect("B 应被接受");
    let deadline = Instant::now() + WAIT;
    loop {
        let requests = server.received_requests().await.expect("读取请求失败");
        let saw = requests.iter().any(|r| {
            r.url.path() == "/chat/completions"
                && String::from_utf8_lossy(&r.body).contains(&marker)
        });
        if saw {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "B 的模型请求必须包含 A 的最终回复（最新 Session），不能使用旧快照"
        );
        tokio::time::sleep(POLL).await;
    }
    core.shutdown_join().expect("关闭失败");
}

/// 已接受且已保存的消息 A 在关闭后的归宿（无条件检查，ALR-202/206）：
/// 要么存在于最终会话，要么关闭明确失败——不接受任何其他结果。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_and_saved_message_survives_shutdown() {
    let (env, sid) = TestEnv::new("survive-a");
    let server = wiremock::MockServer::start().await;
    mount_sse(
        &server,
        vec![text_delta_chunk("长时间任务执行中")],
        Some(Duration::from_secs(30)),
    )
    .await;
    let core = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-a", "关闭前正在处理的消息");
    // 等待 A 被接收保存（driver 已确认）。
    let deadline = Instant::now() + WAIT;
    loop {
        let saved = env
            .load_session(&sid)
            .messages
            .iter()
            .any(|m| m.id == "msg-a");
        if saved {
            break;
        }
        assert!(Instant::now() < deadline, "等待 A 保存超时");
        tokio::time::sleep(POLL).await;
    }

    let shutdown = core.shutdown_join();
    let a_in_final = env
        .load_session(&sid)
        .messages
        .iter()
        .any(|m| m.id == "msg-a");
    assert!(
        a_in_final || shutdown.is_err(),
        "已保存的 A 必须在最终会话存活，或关闭明确失败"
    );
}

/// 单槽占用的忙碌拒绝（独立于关闭语义）：无可引导活动轮的间隙，
/// 占位消息之后的投递必须明确返回 Busy。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_rejection_when_slot_occupied_is_explicit() {
    let (env, sid) = TestEnv::new("busy-reject");
    let server = wiremock::MockServer::start().await;
    mount_sse(
        &server,
        vec![text_delta_chunk("挂起中")],
        Some(Duration::from_secs(30)),
    )
    .await;
    let core = core_for(&env, &sid, &server.uri());

    send_message(&core, "msg-a", "第一个问题");
    super::test_support::wait_requests(&server, 1).await;
    core.deliver(AgentInputKind::cancel()).unwrap();
    assert_eq!(
        wait_turn_status(&env, &sid, "msg-a").await,
        tiangong_types::TurnStatus::Cancelled
    );

    // B 占用单槽；C 必须明确 Busy。
    send_message(&core, "msg-b", "第二个问题");
    let c_result = core.deliver(AgentInputKind::prepared_with_id(
        "msg-c",
        vec![tiangong_types::ContentBlock::text("第三个问题")],
    ));
    assert!(
        matches!(c_result, Err(crate::core::CoreError::Busy)),
        "占用期拒绝必须是明确 Busy，当前: {c_result:?}"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 空闲期的工具注入（inject）应被接受并保留（ALR-102/106）：
/// inject 不唤醒 driver、不启动 turn，等待下一次自然 step 边界生效。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_tool_injection_accepted_without_wake() {
    let (env, sid) = TestEnv::new("idle-inject");
    let server = wiremock::MockServer::start().await;
    let core = core_for(&env, &sid, &server.uri());

    let result = core.deliver(AgentInputKind::tool(
        "browser_observation",
        serde_json::json!({"summary": "页面加载完成", "url": "https://example.com"}),
    ));
    assert!(
        result.is_ok(),
        "空闲期 inject 应被 Inbox 接受（等待下一自然 step），当前实现直接拒绝: {result:?}"
    );
    assert!(
        !crate::react::inbox::is_running(&sid),
        "inject 不得唤醒 driver 或启动 turn"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 手动压缩期间的引导消息（steer）不丢失且压缩独立完成（ALR-102/202/104）：
/// 压缩是独立维护活动，不因新输入让路——引导消息转入 Inbox 排队，压缩继续
/// 执行并应用结果，随后同一 driver 自动处理排队的消息（自动顺延）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steer_during_manual_compression_defers_to_next_turn() {
    let (env, sid) = TestEnv::new("steer-compress");
    let server = wiremock::MockServer::start().await;
    let core = core_for(&env, &sid, &server.uri());

    // 预置可压缩历史；压缩响应延迟制造引导窗口（压缩不会被取消）。
    {
        let mut session = env.load_session(&sid);
        session.append_message(crate::session::MessageRole::User, "第一条问题");
        session.append_message(crate::session::MessageRole::User, "第二条问题");
        session.try_persist_to_disk().expect("预置历史失败");
    }
    mount_completion(
        &server,
        "[[SUMMARY]]\n压缩完成的历史摘要",
        Some(Duration::from_millis(500)),
    )
    .await;
    // 压缩完成后，排队的引导消息作为下一 turn 的模型请求快速失败（400）。
    mount_permanent_error(&server).await;

    core.deliver(AgentInputKind::compress_context())
        .expect("空闲期手动压缩应被接受");
    // 等待压缩请求发出（进入延迟窗口）。
    let deadline = Instant::now() + WAIT;
    while server.received_requests().await.map_or(0, |r| r.len()) < 1 {
        assert!(Instant::now() < deadline, "等待手动压缩请求超时");
        tokio::time::sleep(POLL).await;
    }

    // 压缩进行期间投递 steer：必须被接受且不丢（排队等压缩完成）。
    let msg_id = format!("{sid}-steer");
    core.deliver(AgentInputKind::prepared_with_id(
        msg_id.clone(),
        vec![tiangong_types::ContentBlock::text("换个方向处理")],
    ))
    .expect("压缩期间的 steer 应被接受");

    // 压缩完成后 driver 自动处理排队的消息（无需再投递任何输入）。
    let deadline = Instant::now() + WAIT;
    loop {
        let settled = env
            .load_session(&sid)
            .messages
            .iter()
            .any(|m| m.id == msg_id && m.turn_status.is_some());
        if settled {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "压缩期间的引导消息应在压缩完成后自动顺延执行"
        );
        tokio::time::sleep(POLL).await;
    }
    // 压缩没有被引导取消：摘要已应用。
    let session = env.load_session(&sid);
    assert!(
        session.context_summary.is_some(),
        "压缩是独立维护活动，不应因引导消息取消"
    );
    core.shutdown_join().expect("关闭失败");
}
