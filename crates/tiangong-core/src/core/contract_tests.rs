//! 新契约行为测试：Inbox、单 driver、最新 Session 与关闭可靠性（requirements.md §5.2）。
//!
//! 本文件针对 `TiangongCore::deliver` 公开边界断言目标行为（ALR-101~106、201~206），
//! 不固化当前的临时下一轮队列和每消息后台任务等内部实现。大部分用例在当前过渡
//! 实现上**预期失败**，以 `#[ignore]` 标注启用任务：它们证明旧交接方案的
//! “合并轮次 / 旧 Session 快照 / 关闭静默丢消息 / 空闲注入被拒”缺陷，并守护
//! 任务 14 的 Agent Inbox 与唯一 driver 不被回退。启用任务完成后移除 ignore，
//! 测试必须转绿。
//!
//! 通过 `cargo test -p tiangong-core -- --ignored` 可单独运行并核对失败形态。

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

/// 挂载一个永久匹配的 400 请求错误（4xx 不重试），让每个 turn 快速失败结束。
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

/// 等待 `sid` 的活跃 turn 与下一轮队列全部排空。
async fn wait_idle(sid: &str) {
    let deadline = Instant::now() + WAIT;
    while (crate::shared_runtime::is_running(sid) || crate::shared_runtime::has_next_turn(sid))
        && Instant::now() < deadline
    {
        tokio::time::sleep(POLL).await;
    }
}

/// 空闲期快速连发的多条用户消息应按 FIFO 各成一个 turn（ALR-101/104）：
/// 每条消息在磁盘 session 上各自获得独立的 turn 终态，由同一 driver 顺序执行。
///
/// 当前实现：后续消息经命令通道并入当前轮重启，最终只有一条消息携带 turn
/// 状态（其余被合并），正是要消除的合并轮次行为。
#[ignore = "任务 14：Agent Inbox next_turn FIFO 实现后启用"]
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

    wait_idle(&sid).await;
    // 排空后额外等待，确认没有延迟启动的后续 turn。
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !crate::shared_runtime::is_running(&sid),
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
/// 当前实现：封口时提前 build_turn_context 捕获旧 Session 快照，旧轮之后
/// 落盘的最终消息对下一轮模型请求不可见。
#[ignore = "任务 14：下一 turn 从最新 Session 构建实现后启用"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_next_turn_reads_latest_session() {
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("latest-session-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    mount_permanent_error(&server).await;
    let core = core_for(root.path(), &sid, &server.uri());

    // 挂起一个占用注册表的旧轮（release 控制结束），并进入封口态。
    let (mut ctx, _) = crate::shared_runtime::dummy_context();
    ctx.session.id = sid.clone();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    crate::shared_runtime::spawn_turn(ctx, move |_ctx, cmd_rx| {
        Ok(async move {
            let _cmd_rx = cmd_rx;
            let _ = release_rx.await;
        })
    })
    .expect("spawn 旧轮失败");
    crate::shared_runtime::begin_seal(&sid);

    // 封口窗口投递下一条用户消息：入队即接受。
    core.deliver(AgentInputKind::prepared_with_id(
        format!("{sid}-next"),
        vec![tiangong_types::ContentBlock::text("请基于上一轮结果继续")],
    ))
    .expect("封口窗口消息应被接受");

    // 模拟旧轮（turn A）最终提交落盘：写入带唯一标记的最终 assistant 消息。
    let marker = format!("SEALED-TURN-A-FINAL-{sid}");
    {
        let mut session = Session::load_from_storage(root.path(), &sid).expect("加载失败");
        session.append_message(crate::session::MessageRole::Assistant, &marker);
        session.try_persist_to_disk().expect("旧轮最终落盘失败");
    }

    // 释放旧轮：交接启动下一 turn，其模型请求打到 mock。
    release_tx.send(()).expect("释放旧轮失败");
    wait_idle(&sid).await;

    let requests = server.received_requests().await.expect("读取请求失败");
    let saw_marker = requests.iter().any(|request| {
        request.url.path() == "/chat/completions"
            && String::from_utf8_lossy(&request.body).contains(&marker)
    });
    assert!(
        saw_marker,
        "下一 turn 的模型请求必须包含前一 turn 最终提交的消息（最新 Session），不能使用旧快照"
    );
    core.shutdown_join().expect("关闭失败");
}

/// 关闭不得静默丢弃已确认接受的消息（ALR-202/206）：
/// deliver 已返回 Ok 的消息，在关闭后必须能在磁盘 session 找到（可恢复），
/// 或关闭本身返回明确失败；且关闭后不得再启动新 turn、发出新的模型请求。
///
/// 当前实现：关闭路径 clear_next_turn 直接清空已接受队列，磁盘无消息且
/// shutdown 返回 Ok——静默丢弃。
#[ignore = "任务 14：关闭可靠性（Inbox 持久化或明确失败）实现后启用"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_does_not_silently_drop_accepted_message() {
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let sid = format!("shutdown-{}", scru128::new_string());
    let server = wiremock::MockServer::start().await;
    let core = core_for(root.path(), &sid, &server.uri());

    // 挂起一个读取命令的旧轮：收到 Cancel（关闭路径强制投递）即退出。
    let (mut ctx, _) = crate::shared_runtime::dummy_context();
    ctx.session.id = sid.clone();
    crate::shared_runtime::spawn_turn(ctx, move |_ctx, mut cmd_rx| {
        Ok(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                if matches!(cmd, crate::core::command::Command::Cancel) {
                    break;
                }
            }
        })
    })
    .expect("spawn 旧轮失败");
    crate::shared_runtime::begin_seal(&sid);

    let msg_id = format!("{sid}-accepted");
    core.deliver(AgentInputKind::prepared_with_id(
        msg_id.clone(),
        vec![tiangong_types::ContentBlock::text("关闭前最后一条消息")],
    ))
    .expect("封口窗口消息应被接受（已确认）");

    let shutdown_result = core.shutdown_join();
    // 关闭后等待任何竞态启动的交接任务安定。
    tokio::time::sleep(Duration::from_millis(200)).await;

    let persisted = Session::load_from_storage(root.path(), &sid)
        .map(|session| session.messages.iter().any(|m| m.id == msg_id))
        .unwrap_or(false);
    assert!(
        persisted || shutdown_result.is_err(),
        "已确认接受的消息必须被持久化（可恢复）或让关闭返回明确失败，不得静默丢弃"
    );
    assert!(
        !crate::shared_runtime::is_running(&sid),
        "关闭后不得残留或新启动 turn task"
    );
    let requests = server.received_requests().await.expect("读取请求失败");
    assert!(
        requests.is_empty(),
        "关闭后不得为被丢弃的消息发出任何模型请求"
    );
}

/// 空闲期的工具注入（inject）应被接受并保留（ALR-102/106）：
/// inject 不唤醒 driver、不启动 turn，等待下一次自然 step 边界生效。
///
/// 当前实现：空闲期 send_command 无活跃任务直接失败，deliver 返回
/// WorkerStopped——注入内容丢失。
#[ignore = "任务 14：Agent Inbox next_step（inject 不唤醒）实现后启用"]
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
        !crate::shared_runtime::is_running(&sid),
        "inject 不得唤醒 driver 或启动 turn"
    );
    core.shutdown_join().expect("关闭失败");
}
