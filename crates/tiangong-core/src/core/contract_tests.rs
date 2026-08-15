//! 新契约行为测试：Inbox、单 driver、最新 Session 与关闭可靠性（requirements.md §5.2）。
//!
//! 本文件针对 `TiangongCore::deliver` 公开边界断言目标行为（ALR-101~106、201~206），
//! 不依赖内部调度实现细节。任务 14（Agent Inbox 与唯一 driver）实现后这些用例
//! 必须保持绿色，防止回退到临时队列、每消息后台任务或旧 Session 快照。

use std::time::{Duration, Instant};

use tiangong_types::StreamEvent;

use crate::agent_input::{AgentInput, AgentInputKind};
use crate::core::TiangongCore;
use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::session::Session;

const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(10);

/// 预落盘一个非默认标题的 session（跳过 lite 标题生成，避免干扰 mock 计数），
/// 并构建指向 `endpoint` 的 Core。
fn core_for(root: &std::path::Path, sid: &str, endpoint_base: &str) -> TiangongCore {
    let mut session = Session::new("契约测试会话".to_string());
    session.id = sid.to_string();
    session.bind_storage_root(root);
    session.try_persist_to_disk().expect("预落盘 session 失败");

    let config = CoreConfig::builder()
        .with_chat(endpoint_base, "test-key", "test-model")
        .with_trust_mode(crate::permission::TrustMode::FullTrust)
        .build();
    let (event_tx, _event_rx) = std::sync::mpsc::channel::<StreamEvent>();
    TiangongCore::builder()
        .session_id(sid.to_string())
        .config(CoreConfigProvider::new(config))
        .trust_mode(crate::permission::TrustMode::FullTrust)
        .storage_root(root)
        .workspace_dir(root.to_string_lossy())
        .stream_tx(event_tx)
        .plugins(vec![])
        .build()
}

/// OpenAI SSE chunk（`data: {json}\n\n`）+ 末尾 `[DONE]`。
fn sse_body(chunks: &[serde_json::Value]) -> Vec<u8> {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {}\n\n", chunk));
    }
    body.push_str("data: [DONE]\n\n");
    body.into_bytes()
}

/// 纯文本 delta chunk。
fn text_delta_chunk(content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-contract",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": content},
            "finish_reason": null,
        }],
    })
}

/// 按挂载顺序（FIFO）挂载一条只响应一次的 SSE mock，可带延迟（挂起当前请求）。
async fn mount_sse_with_delay(
    server: &wiremock::MockServer,
    chunks: Vec<serde_json::Value>,
    delay: Option<Duration>,
) {
    let mut response =
        wiremock::ResponseTemplate::new(200).set_body_raw(sse_body(&chunks), "text/event-stream");
    if let Some(delay) = delay {
        response = response.set_delay(delay);
    }
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer test-key",
        ))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

/// 挂载一条只响应一次的非流式完成响应（手动压缩用），可带延迟。
async fn mount_completion_with_delay(
    server: &wiremock::MockServer,
    content: &str,
    delay: Option<Duration>,
) {
    let body = serde_json::json!({
        "id": "chatcmpl-manual-compress",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
    });
    let mut response = wiremock::ResponseTemplate::new(200).set_body_json(body);
    if let Some(delay) = delay {
        response = response.set_delay(delay);
    }
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer test-key",
        ))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

/// 挂载一个永久匹配的 400 请求错误（4xx 不重试），让 turn 快速失败结束。
async fn mount_permanent_error(server: &wiremock::MockServer) {
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer test-key",
        ))
        .respond_with(
            wiremock::ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {
                    "message": "contract test force fail",
                    "type": "invalid_request_error",
                    "param": null,
                    "code": "test_error",
                }
            })),
        )
        .mount(server)
        .await;
}

/// 下一轮从最新 Session 构建（ALR-201/204）：turn A 提交落盘后投递的
/// 消息 B，其模型请求必须包含 A 的最终回复——单槽交接下 driver 完成当前
/// 轮后才领取下一条输入。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_next_turn_reads_latest_session() {
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("latest-session-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    let core = core_for(root.path(), &sid, &server.uri());

    // turn A 直接文本完成（带唯一标记），随后 B 的请求快速失败（可捕获 body）。
    let marker = format!("SEALED-TURN-A-FINAL-{sid}");
    mount_sse_with_delay(&server, vec![text_delta_chunk(&marker)], None).await;
    mount_permanent_error(&server).await;

    core.deliver(AgentInputKind::prepared_with_id(
        format!("{sid}-a"),
        vec![tiangong_types::ContentBlock::text("第一条消息")],
    ))
    .expect("A 应被接受");
    let a_id = format!("{sid}-a");
    let deadline = Instant::now() + WAIT;
    loop {
        let settled = Session::load_from_storage(root.path(), &sid)
            .map(|s| {
                s.messages
                    .iter()
                    .any(|m| m.id == a_id && m.turn_status.is_some())
            })
            .unwrap_or(false);
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

/// 关闭不得静默丢弃已确认接受的消息（ALR-202/206）：deliver 返回 Ok 的
/// 消息必须持久化（可恢复）或让关闭返回明确失败；返回 Busy 的是明确拒绝
///（排队归 app 层），无接受即无丢失。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_does_not_silently_drop_accepted_message() {
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("shutdown-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    let core = core_for(root.path(), &sid, &server.uri());

    // turn A 的模型响应长时间挂起（模拟长任务），关闭时被强制取消。
    mount_sse_with_delay(
        &server,
        vec![text_delta_chunk("长时间任务执行中")],
        Some(Duration::from_secs(30)),
    )
    .await;

    core.deliver(AgentInputKind::prepared_with_id(
        format!("{sid}-accepted"),
        vec![tiangong_types::ContentBlock::text("关闭前正在处理的消息")],
    ))
    .expect("消息 A 应被接受（已确认）");
    // 运行中的第二条：被接受（引导）或被明确拒绝（Busy）——两者都不丢。
    let b_result = core.deliver(AgentInputKind::prepared_with_id(
        format!("{sid}-pending"),
        vec![tiangong_types::ContentBlock::text("排队未处理的消息 B")],
    ));

    let shutdown_result = core.shutdown_join();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let b_accepted = b_result.is_ok();
    if b_accepted {
        let persisted = Session::load_from_storage(root.path(), &sid)
            .map(|s| s.messages.iter().any(|m| m.id == format!("{sid}-pending")))
            .unwrap_or(false);
        assert!(
            persisted || shutdown_result.is_err(),
            "已确认接受的消息必须被持久化（可恢复）或让关闭返回明确失败"
        );
    } else {
        assert!(
            matches!(b_result, Err(crate::core::CoreError::Busy)),
            "拒绝必须是明确的 Busy（排队归 app 层），当前: {:?}",
            b_result
        );
    }
    assert!(
        !crate::react::inbox::is_running(&sid),
        "关闭后不得残留或新启动 turn task"
    );
    let requests = server.received_requests().await.expect("读取请求失败");
    assert!(
        requests.iter().all(|r| {
            !String::from_utf8_lossy(&r.body).contains("排队未处理的消息 B") || b_accepted && false
        }) || !b_accepted
            || Session::load_from_storage(root.path(), &sid)
                .map(|s| s.messages.iter().any(|m| m.id == format!("{sid}-pending")))
                .unwrap_or(false),
        "消息 B 要么已保存，要么从未被接受"
    );
}

/// 空闲期的工具注入（inject）应被接受并保留（ALR-102/106）：
/// inject 不唤醒 driver、不启动 turn，等待下一次自然 step 边界生效。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_tool_injection_accepted_without_wake() {
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("idle-inject-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    let core = core_for(root.path(), &sid, &server.uri());

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
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("steer-compress-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    let core = core_for(root.path(), &sid, &server.uri());

    // 预置可压缩历史；压缩响应延迟制造引导窗口（压缩不会被取消）。
    {
        let mut session = Session::load_from_storage(root.path(), &sid).expect("加载失败");
        session.append_message(crate::session::MessageRole::User, "第一条问题");
        session.append_message(crate::session::MessageRole::User, "第二条问题");
        session.try_persist_to_disk().expect("预置历史失败");
    }
    mount_completion_with_delay(
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
        let settled = Session::load_from_storage(root.path(), &sid)
            .map(|session| {
                session
                    .messages
                    .iter()
                    .any(|m| m.id == msg_id && m.turn_status.is_some())
            })
            .unwrap_or(false);
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
    let session = Session::load_from_storage(root.path(), &sid).expect("加载失败");
    assert!(
        session.context_summary.is_some(),
        "压缩是独立维护活动，不应因引导消息取消"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 运行中到达的消息按引导处理（ALR-102）：中止当前模型请求、保存新消息、
/// 从新意图重启同一轮；多条输入全部保存，最后一条为当前意图。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_message_steers_current_turn_and_saves_all() {
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("steer-turn-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    let core = core_for(root.path(), &sid, &server.uri());

    // turn A 的模型响应长时间挂起制造引导窗口；重启后的请求快速失败结束本轮。
    mount_sse_with_delay(
        &server,
        vec![text_delta_chunk("长时间任务执行中")],
        Some(Duration::from_secs(30)),
    )
    .await;
    mount_permanent_error(&server).await;

    core.deliver(AgentInputKind::prepared_with_id(
        "steer-msg-0",
        vec![tiangong_types::ContentBlock::text("第一条消息")],
    ))
    .expect("第一条消息应被接受");
    // 等 turn A 的模型请求真正发出（挂起中）——投递唤醒即置 Running，但
    // 引导窗口从轮次建立命令通道（begin_turn）开始；单槽占用期的消息会
    // 被明确拒绝（Busy，排队归 app 层）。
    let deadline = Instant::now() + WAIT;
    while server.received_requests().await.map_or(0, |r| r.len()) < 1 {
        assert!(Instant::now() < deadline, "等待 turn A 请求发出超时");
        tokio::time::sleep(POLL).await;
    }

    // 运行中连投两条（引导）：中止当前请求并从最新意图重启。
    for (id, text) in [("steer-msg-1", "中途修正"), ("steer-msg-2", "最终方向")] {
        core.deliver(AgentInputKind::prepared_with_id(
            id,
            vec![tiangong_types::ContentBlock::text(text)],
        ))
        .expect("运行中消息应按引导被接受");
    }

    // 引导中止挂起的请求并重启；重启后的请求快速失败 → 轮结束。
    let deadline = Instant::now() + WAIT;
    while crate::react::inbox::is_running(&sid) && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
    }

    let session = Session::load_from_storage(root.path(), &sid).expect("加载失败");
    for id in ["steer-msg-1", "steer-msg-2"] {
        assert!(
            session.messages.iter().any(|m| m.id == id),
            "引导消息 {id} 应保存进 session"
        );
    }
    core.shutdown_join().expect("关闭失败");
}
