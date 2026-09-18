//! 模型切换编排的端到端测试。
//!
//! 覆盖方案要求的场景：相同模型直接发送、不同模型先压缩再切换、目标
//! 模型不存在、失效引用、压缩失败、切换失败、默认模型变化、Core 重建
//! 后从 Session 恢复。

use std::sync::Arc;

use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tiangong_core::config::core::{CoreConfig, CoreConfigProvider};
use tiangong_core::core::Plugin;
use tiangong_core::permission::TrustMode;
use tiangong_core::session::{MessageRole, Session};
use tiangong_core_manager::CoreManager;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn manager_at(dir: &tempfile::TempDir) -> CoreManager {
    CoreManager::new(
        CoreConfigProvider::new(CoreConfig::default()),
        dir.path().to_path_buf(),
    )
}

/// 写入 models.json：两个模型 a/b 指向同一个 mock server，chat 路由默认指向 a。
fn seed_models(dir: &tempfile::TempDir, base_url: &str, default_key: &str) {
    let config = serde_json::json!({
        "providers": {
            "p": {
                "base_url": base_url,
                "api_key": "test-key",
                "timeout_ms": 60000,
                "protocol": "openai_chat_completions",
                "headers": {}
            }
        },
        "models": {
            "model-a": {
                "provider": "p",
                "model": "model-a",
                "capabilities": ["chat"]
            },
            "model-b": {
                "provider": "p",
                "model": "model-b",
                "capabilities": ["chat"]
            },
            "embed-only": {
                "provider": "p",
                "model": "embed-only",
                "capabilities": ["embedding"]
            }
        },
        "routing": {
            "chat": default_key
        }
    });
    std::fs::write(
        dir.path().join("models.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

/// 预置带两轮问答历史的会话（压缩需要可折叠的历史）。
fn seed_session(dir: &tempfile::TempDir, id: &str, model_ref: Option<&str>) {
    let mut session = Session::new("切换测试");
    session.id = id.to_string();
    session.bind_storage_root(dir.path().to_path_buf());
    for round in [("第一问", "第一答"), ("第二问", "第二答")] {
        session.append_message(MessageRole::User, round.0);
        session.append_message(MessageRole::Assistant, round.1);
    }
    session.model_ref = model_ref.map(str::to_string);
    session.try_persist_to_disk().unwrap();
}

async fn make_core(manager: &CoreManager, id: &str) {
    let (stream_tx, _stream_rx) = std::sync::mpsc::channel();
    manager
        .ensure_core(
            id,
            CoreConfig::builder()
                .with_trust_mode(TrustMode::FullTrust)
                .build(),
            "/tmp".to_string(),
            stream_tx,
            Vec::<Arc<dyn Plugin>>::new,
        )
        .await
        .expect("ensure_core 失败");
}

fn summary_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": "[[SUMMARY]]\n切换前摘要"}}],
        "usage": {"prompt_tokens": 40, "completion_tokens": 8, "total_tokens": 48}
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 目标与当前模型相同时不压缩不切换() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    seed_models(&dir, &server.uri(), "model-a");
    seed_session(&dir, "same-model", Some("model-a"));
    let manager = manager_at(&dir);
    make_core(&manager, "same-model").await;

    let target = manager.resolve_turn_model(Some("model-a")).unwrap();
    manager
        .switch_model_if_needed("same-model", target)
        .await
        .expect("相同模型不应失败");

    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "模型未变化时不得触发压缩"
    );
    manager.retire_core("same-model", true).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 模型不同时先压缩再切换() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(summary_reply())
        .mount(&server)
        .await;
    seed_models(&dir, &server.uri(), "model-a");
    seed_session(&dir, "switch-ok", Some("model-a"));
    let manager = manager_at(&dir);
    make_core(&manager, "switch-ok").await;

    let before = {
        let registry = manager.registry();
        registry.get("switch-ok").unwrap().current_endpoint().model
    };
    assert_eq!(before, "model-a", "初始模型来自 Session.model_ref");

    let target = manager.resolve_turn_model(Some("model-b")).unwrap();
    manager
        .switch_model_if_needed("switch-ok", target)
        .await
        .expect("切换应成功");

    // 压缩发生（摘要边界推进）。
    let session = manager.load_session("switch-ok").unwrap();
    assert!(session.summary_up_to > 0, "切换前应先整理上下文");
    assert_eq!(
        session.context_summary.as_deref(),
        Some("切换前摘要"),
        "摘要由旧模型产出"
    );
    // 模型已切换。
    let after = {
        let registry = manager.registry();
        registry.get("switch-ok").unwrap().current_endpoint().model
    };
    assert_eq!(after, "model-b");
    manager.retire_core("switch-ok", true).await.unwrap();
}

/// 压缩失败不得阻断模型切换。
///
/// 旧模型额度耗尽或服务下线时压缩必然失败；若把它当作整体失败，用户将
/// 永远换不掉模型。压缩只是尽力而为，失败仅记录警告。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 压缩失败仍继续切换模型() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    // 压缩恒失败。
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    seed_models(&dir, &server.uri(), "model-a");
    seed_session(&dir, "compact-fail", Some("model-a"));
    let manager = manager_at(&dir);
    make_core(&manager, "compact-fail").await;

    let target = manager.resolve_turn_model(Some("model-b")).unwrap();
    manager
        .switch_model_if_needed("compact-fail", target)
        .await
        .expect("压缩失败不应阻断模型切换");

    // 压缩确实尝试过且失败（摘要边界未推进）。
    let session = manager.load_session("compact-fail").unwrap();
    assert_eq!(session.summary_up_to, 0, "压缩失败不应推进摘要边界");
    assert!(
        !server.received_requests().await.unwrap().is_empty(),
        "应实际尝试过压缩"
    );

    let current = {
        let registry = manager.registry();
        registry
            .get("compact-fail")
            .unwrap()
            .current_endpoint()
            .model
    };
    assert_eq!(current, "model-b", "压缩失败后仍应切换到目标模型");
    manager.retire_core("compact-fail", true).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 无可整理历史时零模型调用直接切换() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    seed_models(&dir, &server.uri(), "model-a");
    // 空会话：没有可折叠的历史。
    let mut session = Session::new("空会话");
    session.id = "empty-history".to_string();
    session.bind_storage_root(dir.path().to_path_buf());
    session.model_ref = Some("model-a".to_string());
    session.try_persist_to_disk().unwrap();
    let manager = manager_at(&dir);
    make_core(&manager, "empty-history").await;

    let target = manager.resolve_turn_model(Some("model-b")).unwrap();
    manager
        .switch_model_if_needed("empty-history", target)
        .await
        .expect("无历史时切换应成功");

    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "无可整理历史不得调用模型"
    );
    let current = {
        let registry = manager.registry();
        registry
            .get("empty-history")
            .unwrap()
            .current_endpoint()
            .model
    };
    assert_eq!(current, "model-b");
    manager.retire_core("empty-history", true).await.unwrap();
}

#[test]
fn 目标模型不存在或不支持对话时解析失败() {
    let dir = tempfile::tempdir().unwrap();
    seed_models(&dir, "http://unused.invalid", "model-a");
    let manager = manager_at(&dir);

    let missing = manager
        .resolve_turn_model(Some("no-such-model"))
        .expect_err("不存在的模型应报错");
    assert!(missing.contains("已不在配置中"), "{missing}");

    let not_chat = manager
        .resolve_turn_model(Some("embed-only"))
        .expect_err("非 Chat 模型应报错");
    assert!(not_chat.contains("不支持对话"), "{not_chat}");
}

#[test]
fn 默认模型变化时目标随之变化() {
    let dir = tempfile::tempdir().unwrap();
    seed_models(&dir, "http://unused.invalid", "model-a");
    let manager = manager_at(&dir);

    // None = 跟随默认：解析结果应是当前默认模型。
    let first = manager.resolve_turn_model(None).unwrap();
    assert_eq!(first.model, "model-a");

    // 默认改为 model-b 后，同样的 None 解析出不同目标——这正是不能直接
    // 用 None 与当前模型比较的原因。
    seed_models(&dir, "http://unused.invalid", "model-b");
    let second = manager.resolve_turn_model(None).unwrap();
    assert_eq!(second.model, "model-b");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn core重建后从会话恢复目标模型() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    seed_models(&dir, &server.uri(), "model-a");
    // 会话选择了非默认模型 b。
    seed_session(&dir, "restore-model", Some("model-b"));
    let manager = manager_at(&dir);
    make_core(&manager, "restore-model").await;

    let current = {
        let registry = manager.registry();
        registry
            .get("restore-model")
            .unwrap()
            .current_endpoint()
            .model
    };
    assert_eq!(
        current, "model-b",
        "Core 重建应按 Session.model_ref 恢复实际模型，而非路由默认"
    );

    // 目标与恢复后的当前模型一致 → 不触发压缩。
    let target = manager.resolve_turn_model(Some("model-b")).unwrap();
    manager
        .switch_model_if_needed("restore-model", target)
        .await
        .unwrap();
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "恢复后模型一致，不应重复压缩"
    );
    manager.retire_core("restore-model", true).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 失效引用不静默回退默认且重选有效模型仍识别为切换() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(summary_reply())
        .mount(&server)
        .await;
    seed_models(&dir, &server.uri(), "model-a");
    // 会话引用了一个已从配置中删除的模型。
    seed_session(&dir, "stale-ref", Some("deleted-model"));
    let manager = manager_at(&dir);
    make_core(&manager, "stale-ref").await;

    let current = {
        let registry = manager.registry();
        registry.get("stale-ref").unwrap().current_endpoint()
    };
    assert!(
        !current.is_usable(),
        "失效引用不得静默回退到路由默认，应保持不可用状态由发送路径报错"
    );

    // 发送前解析该失效引用会明确报错。
    let error = manager
        .resolve_turn_model(Some("deleted-model"))
        .expect_err("失效引用必须报错");
    assert!(error.contains("已不在配置中"), "{error}");

    // 用户重选有效模型：与失效的当前模型不同 → 识别为切换。
    // 当前模型不可用，跳过整理（用空端点压缩必然失败，会把用户永久卡死）。
    let target = manager.resolve_turn_model(Some("model-a")).unwrap();
    manager
        .switch_model_if_needed("stale-ref", target)
        .await
        .expect("重选有效模型应成功切换");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "当前模型不可用时不得尝试压缩"
    );
    let after = {
        let registry = manager.registry();
        registry.get("stale-ref").unwrap().current_endpoint().model
    };
    assert_eq!(after, "model-a");
    manager.retire_core("stale-ref", true).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 会话执行中拒绝切换模型() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    // 慢响应：占住 Core。
    Mock::given(method("POST"))
        .respond_with(summary_reply().set_delay(std::time::Duration::from_secs(30)))
        .mount(&server)
        .await;
    seed_models(&dir, &server.uri(), "model-a");
    seed_session(&dir, "busy-switch", Some("model-a"));
    let manager = manager_at(&dir);
    make_core(&manager, "busy-switch").await;

    {
        let registry = manager.registry();
        let core = registry.get("busy-switch").cloned().unwrap();
        drop(registry);
        core.deliver(AgentInputKind::prepared_with_id(
            "m1".to_string(),
            vec![tiangong_types::ContentBlock::text("占用")],
        ))
        .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let target = manager.resolve_turn_model(Some("model-b")).unwrap();
    let error = manager
        .switch_model_if_needed("busy-switch", target)
        .await
        .expect_err("执行中必须拒绝切换");
    assert!(error.contains("正在执行"), "{error}");

    let current = {
        let registry = manager.registry();
        registry
            .get("busy-switch")
            .unwrap()
            .current_endpoint()
            .model
    };
    assert_eq!(current, "model-a", "拒绝切换后保持旧模型");
    manager.retire_core("busy-switch", true).await.unwrap();
}

/// 两个 provider 提供同名模型时，切换必须被识别。
///
/// 回归：若用模型名当身份（或只比模型名），此场景会被判为「未变化」，
/// 结果是新端点不生效、切换前的上下文整理也被跳过。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 同名模型跨服务端切换仍先压缩再切换() {
    let dir = tempfile::tempdir().unwrap();
    let old_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(summary_reply())
        .mount(&old_server)
        .await;
    let new_server = MockServer::start().await;

    // 两个 provider 的模型名完全相同，只有服务端地址不同。
    let config = serde_json::json!({
        "providers": {
            "old": {
                "base_url": old_server.uri(),
                "api_key": "test-key",
                "timeout_ms": 60000,
                "protocol": "openai_chat_completions",
                "headers": {}
            },
            "new": {
                "base_url": new_server.uri(),
                "api_key": "test-key",
                "timeout_ms": 60000,
                "protocol": "openai_chat_completions",
                "headers": {}
            }
        },
        "models": {
            "same-on-old": {"provider": "old", "model": "qwen3-max", "capabilities": ["chat"]},
            "same-on-new": {"provider": "new", "model": "qwen3-max", "capabilities": ["chat"]}
        },
        "routing": {"chat": "same-on-old"}
    });
    std::fs::write(
        dir.path().join("models.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    seed_session(&dir, "same-name", Some("same-on-old"));

    let manager = manager_at(&dir);
    make_core(&manager, "same-name").await;

    let target = manager.resolve_turn_model(Some("same-on-new")).unwrap();
    manager
        .switch_model_if_needed("same-name", target)
        .await
        .expect("同名不同服务端应识别为切换");

    let session = manager.load_session("same-name").unwrap();
    assert!(
        session.summary_up_to > 0,
        "同名模型跨服务端切换同样要先整理上下文"
    );
    assert!(
        !old_server.received_requests().await.unwrap().is_empty(),
        "整理上下文应由旧服务端完成"
    );
    let current = {
        let registry = manager.registry();
        registry.get("same-name").unwrap().current_endpoint()
    };
    assert_eq!(current.model, "qwen3-max");
    assert_eq!(current.base_url, new_server.uri(), "端点应指向新服务端");
    manager.retire_core("same-name", true).await.unwrap();
}

/// 两个注册表条目指向同一服务端的同一模型时，不得判为切换。
///
/// 端点身份由 `base_url + model` 派生：换 key（甚至换 provider 名）但实际
/// 模型没变，重复压缩会白白丢失上下文细节并多花一次模型调用。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 同一模型换注册表条目不触发压缩与切换() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let config = serde_json::json!({
        "providers": {
            "p": {
                "base_url": server.uri(),
                "api_key": "test-key",
                "timeout_ms": 60000,
                "protocol": "openai_chat_completions",
                "headers": {}
            }
        },
        "models": {
            "alias-one": {"provider": "p", "model": "gpt-4o", "capabilities": ["chat"]},
            "alias-two": {"provider": "p", "model": "gpt-4o", "capabilities": ["chat"]}
        },
        "routing": {"chat": "alias-one"}
    });
    std::fs::write(
        dir.path().join("models.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    seed_session(&dir, "alias-switch", Some("alias-one"));

    let manager = manager_at(&dir);
    make_core(&manager, "alias-switch").await;

    let target = manager.resolve_turn_model(Some("alias-two")).unwrap();
    manager
        .switch_model_if_needed("alias-switch", target)
        .await
        .expect("同一模型换条目不应失败");

    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "实际模型未变，不得触发压缩"
    );
    let session = manager.load_session("alias-switch").unwrap();
    assert_eq!(session.summary_up_to, 0, "实际模型未变，上下文应原样保留");
    manager.retire_core("alias-switch", true).await.unwrap();
}

/// 切换必须等压缩进入终态后才发生。
///
/// 若压缩刚启动就切换，会与压缩任务并发读写 Session，或撞上 `Busy`。
/// 用慢响应把压缩拖住：`switch_model_if_needed` 返回时 Core 必须已空闲，
/// 且切换确实生效。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 切换在压缩进入终态后才发生() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(summary_reply().set_delay(std::time::Duration::from_millis(700)))
        .mount(&server)
        .await;
    seed_models(&dir, &server.uri(), "model-a");
    seed_session(&dir, "await-terminal", Some("model-a"));
    let manager = manager_at(&dir);
    make_core(&manager, "await-terminal").await;

    let target = manager.resolve_turn_model(Some("model-b")).unwrap();
    manager
        .switch_model_if_needed("await-terminal", target)
        .await
        .expect("切换应成功");

    let core = {
        let registry = manager.registry();
        registry.get("await-terminal").cloned().unwrap()
    };
    assert!(
        !core.is_busy(),
        "返回时压缩任务必须已进入终态，任务槽已释放"
    );
    assert_eq!(core.current_endpoint().model, "model-b");
    let session = manager.load_session("await-terminal").unwrap();
    assert!(session.summary_up_to > 0, "慢压缩应等到收敛而不是被跳过");
    manager.retire_core("await-terminal", true).await.unwrap();
}

/// 切换前压缩使用目标模型的上下文窗口。
///
/// 压缩产物最终交给目标模型，按目标窗口裁剪更贴合；目标未声明窗口时
/// 回落到会话配置的通用限制。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 目标模型的上下文窗口透传到端点() {
    let dir = tempfile::tempdir().unwrap();
    let config = serde_json::json!({
        "providers": {
            "p": {
                "base_url": "http://unused.invalid",
                "api_key": "test-key",
                "timeout_ms": 60000,
                "protocol": "openai_chat_completions",
                "headers": {}
            }
        },
        "models": {
            "narrow": {
                "provider": "p",
                "model": "narrow-model",
                "capabilities": ["chat"],
                "context_window": 32768
            },
            "unspecified": {"provider": "p", "model": "plain-model", "capabilities": ["chat"]}
        },
        "routing": {"chat": "narrow"}
    });
    std::fs::write(
        dir.path().join("models.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    let manager = manager_at(&dir);

    let narrow = manager.resolve_turn_model(Some("narrow")).unwrap();
    assert_eq!(
        narrow.context_window,
        Some(32768),
        "注册表声明的窗口必须透传到端点，供切换前压缩使用"
    );
    // 窗口不属于模型身份：仅窗口不同不触发切换。
    let mut same_model_wider = narrow.clone();
    same_model_wider.context_window = Some(200_000);
    assert!(narrow.is_same_model(&same_model_wider));

    let unspecified = manager.resolve_turn_model(Some("unspecified")).unwrap();
    assert_eq!(
        unspecified.context_window, None,
        "未声明窗口时留空，由调用方回落到会话配置"
    );
}
