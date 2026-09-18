//! 配置交接**编排**：识别待交接标记后经 core 既有的手动压缩能力整理
//! 上下文，再让新配置接管。
//!
//! 状态记账（标记表/首检记录/定档指纹）在 manager（`core_manager::
//! config_handoff`），这里做决策与执行：变化点打标后的即时处理（空闲即
//! 压、忙则留标记）、投递消息前的快查与首检兜底、压缩发起与收敛判据。
//! core 只保留通用压缩能力（手动压缩命令），不感知「交接」概念。
//!
//! 两个变化入口在宿主：插件装卸/升级/启停/回滚/重载统一挂在
//! `notify_plugins_changed`（插件命令的公共成功尾部）；模型切换挂在
//! `sync_core_config_from_state`（比对前后模板端点）。

use tiangong_core::agent_input::{AgentInput, AgentInputKind, CommandInput};
use tiangong_core::core::CoreError;
use tiangong_core::session::{MessageRole, Session};
use tiangong_core_manager::core_manager::config_handoff::ConfigHandoffOutcome;
use tiangong_core_manager::CoreManager;

/// 等待交接压缩收敛的上限：压缩调用通常数秒到数十秒完成；超时后消息
/// 照常投递（进行中的压缩会被用户消息按既有语义取消并起新轮）。
const HANDOFF_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);
/// 等待收敛的轮询间隔。
const HANDOFF_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// 变化点入口：给全部活跃会话打上待交接标记并立即尝试处理——空闲会话
/// 当场压缩，忙会话留标记等投递路径。处理在后台并行进行，立即返回。
///
/// 不活跃会话（无 Core）不打标：其配置变化由首检兜底发现。
pub(crate) fn mark_and_process(manager: &CoreManager, target_fingerprint: &str) {
    let session_ids: Vec<String> = {
        let registry = manager.registry();
        registry.iter().map(|(id, _)| id.clone()).collect()
    };
    if session_ids.is_empty() {
        return;
    }
    manager.set_pending_handoff(&session_ids, target_fingerprint);
    tracing::info!(
        sessions = session_ids.len(),
        "执行配置已变化，活跃会话标记待交接并开始处理"
    );
    // 有 runtime 上下文（含 tauri async runtime 线程）时立即并行处理；
    // 否则只留标记，由投递路径处理。
    let target = target_fingerprint.to_string();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        for id in session_ids {
            let manager = manager.clone();
            let target = target.clone();
            handle.spawn(async move {
                if let ConfigHandoffOutcome::Failed(reason) =
                    run_handoff(&manager, &id, &target).await
                {
                    tracing::warn!(session_id = %id, reason, "配置交接压缩未完成，标记保留重试");
                }
            });
        }
    }
}

/// 投递消息前的交接检查：待交接标记命中则执行交接；否则每会话每进程
/// 首检一次指纹兜底（防重启丢标记与漏报的变化点），指纹经 `fingerprint`
/// 惰性求值——首检之外的投递路径零指纹计算。
pub(crate) async fn ensure_before_deliver<F>(
    manager: &CoreManager,
    session_id: &str,
    fingerprint: F,
) -> ConfigHandoffOutcome
where
    F: FnOnce() -> String,
{
    if let Some(target) = manager.pending_handoff(session_id) {
        return run_handoff(manager, session_id, &target).await;
    }
    if manager.handoff_bootstrap_needed(session_id) {
        manager.mark_handoff_checked(session_id);
        let current = fingerprint();
        if manager.pinned_fingerprint_matches(session_id, &current) {
            return ConfigHandoffOutcome::Aligned;
        }
        return run_handoff(manager, session_id, &current).await;
    }
    ConfigHandoffOutcome::Aligned
}

/// 执行一次交接：无可交接历史直接定档；否则注入手动压缩并等待收敛，
/// 摘要边界推进才算成功并定档。失败/忙写入待交接标记供投递路径重试。
pub(crate) async fn run_handoff(
    manager: &CoreManager,
    session_id: &str,
    target: &str,
) -> ConfigHandoffOutcome {
    let outcome = run_handoff_inner(manager, session_id, target).await;
    if !matches!(outcome, ConfigHandoffOutcome::Aligned) {
        // 未完成：写入/保留待交接标记——变化点路径本就有标记（幂等），
        // 首检兜底路径据此获得后续投递前的重试入口。
        manager.set_pending_handoff(std::slice::from_ref(&session_id.to_string()), target);
    }
    outcome
}

async fn run_handoff_inner(
    manager: &CoreManager,
    session_id: &str,
    target: &str,
) -> ConfigHandoffOutcome {
    let session = match manager.load_session(session_id) {
        Ok(session) => session,
        Err(_) => {
            // 会话文件尚不存在（新对话）：没有按旧配置积累的上下文。
            manager.complete_handoff(session_id, target);
            return ConfigHandoffOutcome::Aligned;
        }
    };
    if !has_handable_history(&session) {
        // 边界之后只剩 System/Notice（或历史已折叠为摘要）：直接定档。
        manager.complete_handoff(session_id, target);
        return ConfigHandoffOutcome::Aligned;
    }
    let Some(core) = manager.registry().get(session_id).cloned() else {
        // 标记保留：Core 尚未创建时无法压缩，ensure 之后投递路径会再处理。
        return ConfigHandoffOutcome::Failed("会话无活跃 Core".to_string());
    };
    let summary_before = session.summary_up_to;
    match core.deliver(AgentInputKind::Command(CommandInput::CompressContext)) {
        Ok(()) => {}
        Err(CoreError::Busy) => return ConfigHandoffOutcome::SkippedBusy,
        Err(error) => return ConfigHandoffOutcome::Failed(format!("交接压缩注入失败：{error}")),
    }
    // deliver 返回 Ok 时压缩任务已注册任务槽，is_busy 立即可靠。
    let deadline = tokio::time::Instant::now() + HANDOFF_WAIT_LIMIT;
    while core.is_busy() {
        if tokio::time::Instant::now() >= deadline {
            return ConfigHandoffOutcome::Failed("交接压缩等待超时".to_string());
        }
        tokio::time::sleep(HANDOFF_POLL_INTERVAL).await;
    }
    match manager.load_session(session_id) {
        Ok(after) if after.summary_up_to > summary_before => {
            manager.complete_handoff(session_id, target);
            ConfigHandoffOutcome::Aligned
        }
        Ok(_) => ConfigHandoffOutcome::Failed(
            "交接压缩未推进摘要边界（模型失败或无可压缩内容）".to_string(),
        ),
        Err(error) => ConfigHandoffOutcome::Failed(format!("压缩后回读会话失败：{error}")),
    }
}

/// 摘要边界之后是否还有可交接的历史消息。
///
/// System 与 Notice 不属于对话历史；边界之后只剩这两类（或什么都没有，
/// 即历史已全部折叠为摘要）时，新配置直接生效即可，无需模型调用。
fn has_handable_history(session: &Session) -> bool {
    session
        .messages
        .iter()
        .skip(session.summary_up_to.min(session.messages.len()))
        .any(|message| message.role != MessageRole::System && message.role != MessageRole::Notice)
}

/// 端到端：真 TiangongCore + wiremock 模型端点，验证标记制的识别处理、
/// 首检兜底与成败判定。
#[cfg(test)]
mod handoff_e2e_tests {
    use super::*;
    use std::time::Duration;

    use tiangong_core::config::core::{CoreConfig, CoreConfigProvider};
    use tiangong_core::permission::TrustMode;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_manager(dir: &tempfile::TempDir) -> CoreManager {
        CoreManager::new(
            CoreConfigProvider::new(CoreConfig::default()),
            dir.path().to_path_buf(),
        )
    }

    fn config_for(base_url: &str) -> CoreConfig {
        CoreConfig::builder()
            .with_chat(base_url, "test-key", "test-model")
            .with_trust_mode(TrustMode::FullTrust)
            .build()
    }

    /// 预置带两轮问答历史的会话，并定档一个与目标不一致的旧指纹。
    fn seed_session_with_stale_pin(dir: &tempfile::TempDir, manager: &CoreManager, id: &str) {
        let mut session = Session::new("交接测试");
        session.id = id.to_string();
        session.bind_storage_root(dir.path().to_path_buf());
        for round in [("第一问", "第一答"), ("第二问", "第二答")] {
            session.append_message(MessageRole::User, round.0);
            session.append_message(MessageRole::Assistant, round.1);
        }
        session.try_persist_to_disk().unwrap();
        manager.complete_handoff(id, "stale-fingerprint");
    }

    fn completion_reply(content: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": content}}],
            "usage": {"prompt_tokens": 40, "completion_tokens": 8, "total_tokens": 48}
        }))
    }

    async fn make_core(manager: &CoreManager, id: &str, base_url: &str) {
        let (stream_tx, _stream_rx) = std::sync::mpsc::channel();
        manager
            .ensure_core(
                id,
                config_for(base_url),
                "/tmp".to_string(),
                stream_tx,
                Vec::new,
            )
            .await
            .expect("ensure_core 失败");
    }

    fn fingerprint_for(base_url: &str) -> String {
        tiangong_core_manager::core_manager::config_handoff::execution_fingerprint(
            &tiangong_llm::ModelEndpoint {
                model: "test-model".into(),
                base_url: base_url.to_string(),
                api_key: "test-key".into(),
                ..Default::default()
            },
            &[],
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 首检兜底发现配置变化并经手动压缩交接() {
        let dir = tempfile::tempdir().unwrap();
        let manager = make_manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n交接摘要"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &manager, "e2e-bootstrap");
        make_core(&manager, "e2e-bootstrap", &server.uri()).await;

        // 首检：定档与当前指纹不一致 → 压缩交接。
        let fp = fingerprint_for(&server.uri());
        let outcome = ensure_before_deliver(&manager, "e2e-bootstrap", || fp.clone()).await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);

        let session = manager.load_session("e2e-bootstrap").unwrap();
        assert_eq!(
            session.context_summary.as_deref(),
            // 压缩器剥离 [[SUMMARY]] 定界标记后落盘摘要正文（core 既有行为）。
            Some("交接摘要")
        );
        assert!(session.summary_up_to > 0, "摘要边界应推进");
        assert!(
            manager.pinned_fingerprint_matches("e2e-bootstrap", &fp),
            "交接成功后定档须更新"
        );
        // 之后的投递路径：首检已做、无标记，惰性指纹不再求值。
        let mut evaluated = false;
        let outcome = ensure_before_deliver(&manager, "e2e-bootstrap", || {
            evaluated = true;
            String::new()
        })
        .await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);
        assert!(!evaluated, "非首检路径不得计算指纹");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        manager.retire_core("e2e-bootstrap", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 交接失败写入标记供投递路径重试() {
        let dir = tempfile::tempdir().unwrap();
        let manager = make_manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("compression unavailable"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &manager, "e2e-retry");
        make_core(&manager, "e2e-retry", &server.uri()).await;

        let fp = fingerprint_for(&server.uri());
        let outcome = ensure_before_deliver(&manager, "e2e-retry", || fp.clone()).await;
        assert!(matches!(outcome, ConfigHandoffOutcome::Failed(_)));
        assert!(
            manager.pending_handoff("e2e-retry").is_some(),
            "失败必须写入待交接标记，投递路径据此重试"
        );
        assert!(
            !manager.pinned_fingerprint_matches("e2e-retry", &fp),
            "失败须保留旧定档"
        );

        // 模型恢复后（新端点+变化点刷新目标），投递路径经标记重试成功。
        drop(server);
        manager.retire_core("e2e-retry", false).await.unwrap();
        let recovered = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n重试摘要"))
            .mount(&recovered)
            .await;
        make_core(&manager, "e2e-retry", &recovered.uri()).await;
        let new_target = fingerprint_for(&recovered.uri());
        mark_and_process(&manager, &new_target);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !manager.pinned_fingerprint_matches("e2e-retry", &new_target) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "变化点后台处理应及时完成交接"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(manager.pending_handoff("e2e-retry"), None);
        manager.retire_core("e2e-retry", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 会话执行中不打断并跳过交接() {
        let dir = tempfile::tempdir().unwrap();
        let manager = make_manager(&dir);
        let server = MockServer::start().await;
        // 压缩响应长时间挂起：占住任务槽，构造「会话忙」。
        Mock::given(method("POST"))
            .respond_with(completion_reply("慢摘要").set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &manager, "e2e-busy");
        make_core(&manager, "e2e-busy", &server.uri()).await;

        // 变化点打标（写标记 + spawn 后台处理）：后台交接的压缩挂起中。
        let fp = fingerprint_for(&server.uri());
        mark_and_process(&manager, &fp);
        tokio::time::sleep(Duration::from_millis(400)).await;
        // 压缩进行中标记在场；投递路径执行：deliver 得 Busy，不打断。
        assert!(manager.pending_handoff("e2e-busy").is_some());
        let second = ensure_before_deliver(&manager, "e2e-busy", || fp.clone()).await;
        assert_eq!(second, ConfigHandoffOutcome::SkippedBusy);

        // 取消挂起的压缩：任务槽释放后后台判据判为未推进 → Failed，标记保留。
        manager.cancel_core("e2e-busy");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while {
            let registry = manager.registry();
            registry.get("e2e-busy").is_some_and(|core| core.is_busy())
        } {
            assert!(
                tokio::time::Instant::now() < deadline,
                "取消后压缩应及时收敛"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            manager.pending_handoff("e2e-busy").is_some(),
            "失败保留标记，投递路径据此重试"
        );
        manager.retire_core("e2e-busy", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 变化点打标后空闲会话立即交接() {
        let dir = tempfile::tempdir().unwrap();
        let manager = make_manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n识别即压摘要"))
            .mount(&server)
            .await;
        // 会话在当前指纹下已定档（模拟上一进程的正常收尾）。
        seed_session_with_stale_pin(&dir, &manager, "e2e-mark");
        make_core(&manager, "e2e-mark", &server.uri()).await;
        manager.complete_handoff("e2e-mark", &fingerprint_for(&server.uri()));

        // 变化点打标（目标=新配置指纹），后台立即处理空闲会话。
        let target = fingerprint_for("https://changed.example.com");
        mark_and_process(&manager, &target);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !manager.pinned_fingerprint_matches("e2e-mark", &target) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "打标后空闲会话应及时完成交接"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(manager.pending_handoff("e2e-mark"), None, "完成后清除标记");
        let session = manager.load_session("e2e-mark").unwrap();
        assert_eq!(session.context_summary.as_deref(), Some("识别即压摘要"));
        manager.retire_core("e2e-mark", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 无可交接历史时直接定档零模型调用() {
        let dir = tempfile::tempdir().unwrap();
        let manager = make_manager(&dir);
        let server = MockServer::start().await;

        // 只有 System/Notice 的会话：新配置直接生效，不调模型。
        let mut session = Session::new("仅系统消息");
        session.id = "system-only".to_string();
        session.bind_storage_root(dir.path().to_path_buf());
        session.messages.push(tiangong_core::session::Message::new(
            MessageRole::Notice,
            "提示",
        ));
        session.try_persist_to_disk().unwrap();
        let fp = fingerprint_for(&server.uri());
        let outcome = ensure_before_deliver(&manager, "system-only", || fp.clone()).await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);
        assert!(manager.pinned_fingerprint_matches("system-only", &fp));
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "无可交接历史不得发起模型调用"
        );

        // 会话文件不存在（新对话）同样直接定档。
        let outcome = ensure_before_deliver(&manager, "fresh", || fp.clone()).await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);
        assert!(manager.pinned_fingerprint_matches("fresh", &fp));
    }
}
