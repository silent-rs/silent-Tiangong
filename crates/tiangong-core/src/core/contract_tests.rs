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

/// 等待磁盘 session 中全部 `ids` 消息都获得 turn 终态（driver 排空的最终信号；
/// 瞬时 is_running=false 不能证明 Inbox 已排空——driver 启动前也存在该窗口）。
async fn wait_all_turns_settled(root: &std::path::Path, sid: &str, ids: &[String]) {
    let deadline = Instant::now() + WAIT;
    loop {
        let settled = Session::load_from_storage(root, sid)
            .map(|session| {
                ids.iter().all(|id| {
                    session
                        .messages
                        .iter()
                        .any(|m| &m.id == id && m.turn_status.is_some())
                })
            })
            .unwrap_or(false);
        if settled {
            return;
        }
        assert!(Instant::now() < deadline, "等待全部 turn 完成超时");
        tokio::time::sleep(POLL).await;
    }
}

/// 空闲期快速连发的多条用户消息应按 FIFO 各成一个 turn（ALR-101/104）：
/// 每条消息在磁盘 session 上各自获得独立的 turn 终态，由同一 driver 顺序执行。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rapid_followups_execute_as_separate_turns() {
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("fifo-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    mount_permanent_error(&server).await;
    let core = core_for(root.path(), &sid, &server.uri());

    let ids: Vec<String> = (0..3).map(|i| format!("{sid}-msg-{i}")).collect();
    for (idx, id) in ids.iter().enumerate() {
        core.deliver(AgentInputKind::prepared_with_id(
            id.clone(),
            vec![tiangong_types::ContentBlock::text(format!(
                "第 {idx} 条独立请求"
            ))],
        ))
        .expect("空闲期投递应被接受");
    }

    wait_all_turns_settled(root.path(), &sid, &ids).await;
    // 全部完成后稍候，确认没有延迟启动的后续 turn。
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !crate::react::inbox::is_running(&sid),
        "全部 followup 处理完毕后应回到空闲"
    );

    let session = Session::load_from_storage(root.path(), &sid).expect("加载 session 失败");
    for (idx, id) in ids.iter().enumerate() {
        let Some(message) = session.messages.iter().find(|m| &m.id == id) else {
            panic!("第 {idx} 条消息应已保存到 session");
        };
        assert!(
            message.turn_status.is_some(),
            "第 {idx} 条 followup 应作为独立 turn 执行并获得终态，当前被并入其他轮"
        );
    }
    core.shutdown_join().expect("关闭失败");
}

/// 封口交接的下一 turn 必须在真正开始时读取最新 Session（ALR-201/204）：
/// 前一 turn 提交完成后的最终数据必须出现在下一 turn 的模型请求中。
///
/// 旧实现：封口时提前 build_turn_context 捕获旧 Session 快照，旧轮之后落盘的
/// 最终消息对下一轮模型请求不可见。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_next_turn_reads_latest_session() {
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("latest-session-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    let core = core_for(root.path(), &sid, &server.uri());

    // turn A 的响应带唯一标记（当前实现经 Summary 阶段保存为最终回复并落盘）。
    let marker = format!("SEALED-TURN-A-FINAL-{sid}");
    mount_sse_with_delay(
        &server,
        vec![text_delta_chunk(&marker)],
        Some(Duration::from_millis(300)),
    )
    .await;
    mount_sse_with_delay(&server, vec![text_delta_chunk(&marker)], None).await;
    // turn B 的请求快速失败（请求体仍可捕获）。
    mount_permanent_error(&server).await;

    // A 执行中投递 B（followup 排队，由同一 driver 在 A 提交后继续执行）。
    core.deliver(AgentInputKind::prepared_with_id(
        format!("{sid}-a"),
        vec![tiangong_types::ContentBlock::text("第一条消息")],
    ))
    .expect("A 应被接受");
    core.deliver(AgentInputKind::prepared_with_id(
        format!("{sid}-b"),
        vec![tiangong_types::ContentBlock::text("请基于上一轮结果继续")],
    ))
    .expect("B 应被接受");

    // 等待 B 的模型请求出现（B 只会在 A 提交落盘后由同一 driver 启动）。
    let deadline = Instant::now() + WAIT;
    loop {
        let requests = server.received_requests().await.expect("读取请求失败");
        let saw_marker = requests.iter().any(|request| {
            request.url.path() == "/chat/completions"
                && String::from_utf8_lossy(&request.body).contains(&marker)
        });
        if saw_marker {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "等待下一 turn 模型请求超时：请求中始终未见前一 turn 的最终消息"
        );
        tokio::time::sleep(POLL).await;
    }
    core.shutdown_join().expect("关闭失败");
}

/// 关闭不得静默丢弃已确认接受的消息（ALR-202/206）：
/// deliver 已返回 Ok 的消息，在关闭后必须能在磁盘 session 找到（可恢复），
/// 或关闭本身返回明确失败；且关闭后不得为被丢弃的消息发出模型请求。
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

    let msg_id = format!("{sid}-accepted");
    core.deliver(AgentInputKind::prepared_with_id(
        msg_id.clone(),
        vec![tiangong_types::ContentBlock::text("关闭前正在处理的消息")],
    ))
    .expect("消息 A 应被接受（已确认）");
    core.deliver(AgentInputKind::prepared_with_id(
        format!("{sid}-pending"),
        vec![tiangong_types::ContentBlock::text("排队未处理的消息 B")],
    ))
    .expect("排队消息 B 应被接受（已确认）");

    let shutdown_result = core.shutdown_join();
    // 关闭后等待任何竞态任务安定。
    tokio::time::sleep(Duration::from_millis(200)).await;

    let persisted = Session::load_from_storage(root.path(), &sid)
        .map(|session| {
            session
                .messages
                .iter()
                .any(|m| m.id == format!("{sid}-pending"))
        })
        .unwrap_or(false);
    assert!(
        persisted || shutdown_result.is_err(),
        "已确认接受的消息必须被持久化（可恢复）或让关闭返回明确失败，不得静默丢弃"
    );
    assert!(
        !crate::react::inbox::is_running(&sid),
        "关闭后不得残留或新启动 turn task"
    );
    let requests = server.received_requests().await.expect("读取请求失败");
    assert!(
        requests.iter().all(|request| {
            !String::from_utf8_lossy(&request.body).contains("排队未处理的消息 B")
        }),
        "关闭后不得为被丢弃的消息发出任何模型请求"
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

/// 手动压缩期间的引导消息（steer）不得丢失（ALR-102/202）：
/// 压缩被取消让路，消息转入 Inbox 作为下一个 turn 自动执行——不需要用户
/// 再发消息触发（自动顺延）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steer_during_manual_compression_defers_to_next_turn() {
    use crate::agent_input::MessageDelivery;

    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("steer-compress-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    let core = core_for(root.path(), &sid, &server.uri());

    // 预置可压缩历史并让手动压缩请求长时间挂起（制造引导窗口）。
    {
        let mut session = Session::load_from_storage(root.path(), &sid).expect("加载失败");
        session.append_message(crate::session::MessageRole::User, "第一条问题");
        session.append_message(crate::session::MessageRole::User, "第二条问题");
        session.try_persist_to_disk().expect("预置历史失败");
    }
    mount_sse_with_delay(
        &server,
        vec![text_delta_chunk("压缩结果")],
        Some(Duration::from_secs(30)),
    )
    .await;
    // 压缩取消后，引导消息作为下一 turn 的模型请求快速失败（400）。
    mount_permanent_error(&server).await;

    core.deliver(AgentInputKind::compress_context())
        .expect("空闲期手动压缩应被接受");
    // 等待压缩请求发出（进入挂起窗口）。
    let deadline = Instant::now() + WAIT;
    while server.received_requests().await.map_or(0, |r| r.len()) < 1 {
        assert!(Instant::now() < deadline, "等待手动压缩请求超时");
        tokio::time::sleep(POLL).await;
    }

    // 压缩挂起期间投递 steer：必须被接受且不丢。
    let msg_id = format!("{sid}-steer");
    core.deliver(AgentInputKind::prepared_with_delivery(
        msg_id.clone(),
        vec![tiangong_types::ContentBlock::text("换个方向处理")],
        MessageDelivery::Steer,
    ))
    .expect("压缩期间的 steer 应被接受");

    // driver 应取消压缩、自动执行该消息（无需再投递任何输入）。
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
            "压缩期间的引导消息应自动顺延执行（转 next_turn 由 driver 领取）"
        );
        tokio::time::sleep(POLL).await;
    }
    core.shutdown_join().expect("关闭失败");
}
