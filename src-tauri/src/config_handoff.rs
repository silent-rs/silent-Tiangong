//! 插件变化上下文交接：插件装卸/升级/启停后，先经 core 既有的手动压缩
//! 能力整理上下文，再让新的插件集合接管。
//!
//! 状态记账与编排都在本模块（app 层）——manager 恪守记账定位（Core
//! 注册表/会话文件/创建锁，issue #245），core 只保留通用压缩能力（手动
//! 压缩命令），两者都不感知「交接」概念。runtime（plugin-runtime）同样
//! 零改动：变化感知在 app 即全覆盖（app 依赖 runtime，插件装卸命令共用
//! `notify_plugins_changed` 尾部）。
//!
//! 运行期是**标记制**：变化发生的地方给活跃会话打标并立即尝试处理
//! （空闲即压，忙则留标记）；投递消息路径只查内存标记。兜底是**首检
//! 指纹比对**：每会话每进程首次投递前算一次插件指纹与落盘定档比对，
//! 防两类漏报——进程重启丢内存标记、未走正常变化入口的变化。定档指纹
//! 持久化在 storage_root 下（不进会话文件）——会话复制/导出后丢失定档
//! 只会重新定档，不触发无谓交接。
//!
//! 指纹输入是插件的「声明态」：id@version + 启用集合，覆盖注册在
//! plugin-runtime 的全部已安装插件（含 prompt 文案插件）——它们都是动态
//! 注册的，装卸/升级/启停一律经 registry，天然全部参与指纹。加载顺序、
//! 运行期抖动（如 sidecar 临时掉线）不改变指纹；反之插件升级即使工具名
//! 不变也会触发交接——历史里按旧版本产生的调用与新版本行为可能已不一致。
//!
//! **压缩尽力而为一次**：交接压缩失败只告警并直接定档，不重试、不阻断
//! 投递。对话一旦继续，KV 缓存已按当前上下文写入，反复重压既无意义又会
//! 让用户每条消息都等待一轮压缩。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tiangong_core::core::CoreError;
use tiangong_core::session::{MessageRole, Session};
use tiangong_core_manager::CoreManager;

/// 定档指纹存储文件（storage_root 下，session_id → 指纹摘要）。
const FINGERPRINTS_FILE: &str = "config-fingerprints.json";

/// 一次交接检查的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigHandoffOutcome {
    /// 配置已对齐：指纹一致、首次定档、无可交接历史或交接完成。
    Aligned,
    /// 会话正在执行 turn：不打断，消息照常投递，标记保留待下一条消息。
    SkippedBusy,
    /// 交接压缩未完成（模型错误/超时）：上下文未整理，已定档不再重试。
    Failed(String),
}

/// 交接内存状态：待处理标记 + 首检记录 + 定档指纹缓存（懒加载）。
#[derive(Default)]
struct HandoffState {
    /// 待交接标记：session_id → 目标指纹（变化点写入；交接完成或放弃后
    /// 清除，仅「忙」时保留供下一条消息重试）。
    pending: HashMap<String, String>,
    /// 本进程已完成首检指纹兜底比对的会话（首检每会话每进程一次）。
    checked: HashSet<String>,
    /// 定档指纹表（session_id → 摘要），None 表示尚未从磁盘加载。
    pinned: Option<HashMap<String, String>>,
}

/// 配置交接状态容器（app 层单例，与 TiangongApp 同生命周期）。
#[derive(Clone)]
pub(crate) struct ConfigHandoffStore {
    state: Arc<Mutex<HandoffState>>,
    storage_root: PathBuf,
}

impl ConfigHandoffStore {
    pub(crate) fn new(storage_root: PathBuf) -> Self {
        Self {
            state: Arc::new(Mutex::new(HandoffState::default())),
            storage_root,
        }
    }

    /// 写入待交接标记（变化点与「忙」时调用；幂等，后写覆盖目标指纹）。
    fn set_pending_handoff(&self, session_id: &str, target_fingerprint: &str) {
        self.lock_state()
            .pending
            .insert(session_id.to_string(), target_fingerprint.to_string());
    }

    /// 读取待交接标记的目标指纹（投递路径快查；无标记返回 None）。
    fn pending_handoff(&self, session_id: &str) -> Option<String> {
        self.lock_state().pending.get(session_id).cloned()
    }

    /// 本会话是否尚未做过首检指纹兜底比对（每会话每进程一次）。
    fn bootstrap_needed(&self, session_id: &str) -> bool {
        !self.lock_state().checked.contains(session_id)
    }

    /// 标记本会话已完成首检。
    fn mark_checked(&self, session_id: &str) {
        self.lock_state().checked.insert(session_id.to_string());
    }

    /// 定档目标指纹（落盘）并清除待交接标记。
    ///
    /// 交接成功与「尽力压缩后放弃」共用本方法——两种情况都意味着当前
    /// 插件集合已经接管，不应再对同一目标重复压缩。
    fn complete_handoff(&self, session_id: &str, fingerprint: &str) {
        let changed = {
            let mut state = self.lock_state();
            state.pending.remove(session_id);
            let pinned = state.pinned.get_or_insert_with(HashMap::new);
            pinned.insert(session_id.to_string(), fingerprint.to_string())
                != Some(fingerprint.to_string())
        };
        if changed {
            self.save_fingerprints();
        }
    }

    /// 定档指纹是否与给定值一致（首检兜底比对的读侧）。
    fn pinned_matches(&self, session_id: &str, fingerprint: &str) -> bool {
        self.lock_state()
            .pinned
            .as_ref()
            .and_then(|pinned| pinned.get(session_id))
            .is_some_and(|p| p == fingerprint)
    }

    /// 移除会话的交接状态（会话删除/物理清理时调用，避免残留脏记录）。
    pub(crate) fn forget(&self, session_id: &str) {
        let removed = {
            let mut state = self.lock_state();
            let from_pinned = state
                .pinned
                .as_mut()
                .is_some_and(|pinned| pinned.remove(session_id).is_some());
            state.checked.remove(session_id)
                || state.pending.remove(session_id).is_some()
                || from_pinned
        };
        if removed {
            self.save_fingerprints();
        }
    }

    /// 状态锁：首次访问时从磁盘懒加载定档表，之后驻留内存。
    fn lock_state(&self) -> std::sync::MutexGuard<'_, HandoffState> {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if guard.pinned.is_none() {
            let loaded = std::fs::read_to_string(self.storage_root.join(FINGERPRINTS_FILE))
                .ok()
                .and_then(|content| serde_json::from_str(&content).ok())
                .unwrap_or_default();
            guard.pinned = Some(loaded);
        }
        guard
    }

    /// 原子落盘（临时文件 + rename）。
    fn save_fingerprints(&self) {
        let snapshot = self.lock_state().pinned.clone().unwrap_or_default();
        let path = self.storage_root.join(FINGERPRINTS_FILE);
        let tmp = self.storage_root.join(format!("{FINGERPRINTS_FILE}.tmp"));
        let write = serde_json::to_string_pretty(&snapshot)
            .map_err(|error| error.to_string())
            .and_then(|content| std::fs::write(&tmp, content).map_err(|error| error.to_string()))
            .and_then(|()| std::fs::rename(&tmp, &path).map_err(|error| error.to_string()));
        if let Err(error) = write {
            tracing::warn!(%error, "定档指纹落盘失败，下次进程内仍可用，重启后丢失");
        }
    }
}

/// 计算插件配置指纹：启用的 runtime 注册插件（id@version）集合。
///
/// 插件输入应是 plugin-runtime 注册表启用集合的声明态——注册在 runtime
/// 的插件都是动态注册的（含 prompt），装卸/升级/启停全部经 registry 变化，
/// 因此无需任何特判。插件键排序去重后参与摘要（加载顺序不是能力变化）；
/// 无版本的插件只记 id。凭据与运行参数（trust_mode/reasoning_effort）不
/// 参与：前者绝不落盘，后者下一轮直接生效、无需交接。
pub(crate) fn execution_fingerprint(plugins: &[(String, String)]) -> String {
    use sha2::{Digest, Sha256};
    let mut plugin_keys: Vec<String> = plugins
        .iter()
        .map(|(id, version)| {
            if version.is_empty() {
                id.clone()
            } else {
                format!("{id}@{version}")
            }
        })
        .collect();
    plugin_keys.sort();
    plugin_keys.dedup();
    let mut hasher = Sha256::new();
    for key in plugin_keys {
        hasher.update(key.as_bytes());
        hasher.update(b"\x1f");
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 变化点入口（插件集合/版本变化）：对每个活跃会话打标并立即尝试处理
/// ——空闲会话当场压缩，忙会话留标记等投递路径。
///
/// 不活跃会话（无 Core）不打标：其变化由首检兜底发现。
pub(crate) fn mark_all_sessions(store: &ConfigHandoffStore, manager: &CoreManager, target: &str) {
    let session_ids: Vec<String> = {
        let registry = manager.registry();
        registry.iter().map(|(id, _)| id.clone()).collect()
    };
    if session_ids.is_empty() {
        return;
    }
    tracing::info!(
        sessions = session_ids.len(),
        "插件配置已变化，活跃会话标记待交接并开始处理"
    );
    for session_id in session_ids {
        mark_session(store, manager, &session_id, target);
    }
}

/// 单会话打标并立即尝试处理：空闲当场压缩，忙则留标记等投递路径。
pub(crate) fn mark_session(
    store: &ConfigHandoffStore,
    manager: &CoreManager,
    session_id: &str,
    target_fingerprint: &str,
) {
    store.set_pending_handoff(session_id, target_fingerprint);
    // 有 runtime 上下文（含 tauri async runtime 线程）时立即处理；
    // 否则只留标记，由投递路径处理。
    let target = target_fingerprint.to_string();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let store = store.clone();
        let manager = manager.clone();
        let id = session_id.to_string();
        handle.spawn(async move {
            run_handoff(&store, &manager, &id, &target).await;
        });
    }
}

/// 投递消息前的交接检查：待交接标记命中则执行交接；否则每会话每进程
/// 首检一次指纹兜底（防重启丢标记与漏报的变化点），指纹经 `fingerprint`
/// 惰性求值——首检之外的投递路径零指纹计算。
pub(crate) async fn ensure_before_deliver<F>(
    store: &ConfigHandoffStore,
    manager: &CoreManager,
    session_id: &str,
    fingerprint: F,
) -> ConfigHandoffOutcome
where
    F: FnOnce() -> String,
{
    if let Some(target) = store.pending_handoff(session_id) {
        return run_handoff(store, manager, session_id, &target).await;
    }
    if store.bootstrap_needed(session_id) {
        store.mark_checked(session_id);
        let current = fingerprint();
        if store.pinned_matches(session_id, &current) {
            return ConfigHandoffOutcome::Aligned;
        }
        return run_handoff(store, manager, session_id, &current).await;
    }
    ConfigHandoffOutcome::Aligned
}

/// 执行一次交接：无可交接历史直接定档；否则注入手动压缩并等待收敛。
///
/// **尽力而为一次**：压缩失败或超时同样定档（新插件集合已接管，历史
/// 未经整理），不保留标记、不重试——对话继续后 KV 缓存已按当前上下文
/// 写入，重压既无意义又会让用户每条消息都等一轮。只有「会话忙」才保留
/// 标记，等下一条消息的空闲窗口。
pub(crate) async fn run_handoff(
    store: &ConfigHandoffStore,
    manager: &CoreManager,
    session_id: &str,
    target: &str,
) -> ConfigHandoffOutcome {
    let outcome = run_handoff_inner(store, manager, session_id, target).await;
    match &outcome {
        ConfigHandoffOutcome::SkippedBusy => {
            // 会话执行中：保留标记，下一条消息的投递路径再试。
            store.set_pending_handoff(session_id, target);
        }
        ConfigHandoffOutcome::Failed(reason) => {
            tracing::warn!(
                session_id,
                reason,
                "交接压缩未完成，历史未经整理即定档（不再重试）"
            );
            store.complete_handoff(session_id, target);
        }
        ConfigHandoffOutcome::Aligned => {}
    }
    outcome
}

async fn run_handoff_inner(
    store: &ConfigHandoffStore,
    manager: &CoreManager,
    session_id: &str,
    target: &str,
) -> ConfigHandoffOutcome {
    let session = match manager.load_session(session_id) {
        Ok(session) => session,
        Err(_) => {
            // 会话文件尚不存在（新对话）：没有按旧配置积累的上下文。
            store.complete_handoff(session_id, target);
            return ConfigHandoffOutcome::Aligned;
        }
    };
    if !has_handable_history(&session) {
        // 边界之后只剩 System/Notice（或历史已折叠为摘要）：直接定档。
        store.complete_handoff(session_id, target);
        return ConfigHandoffOutcome::Aligned;
    }
    let Some(core) = manager.registry().get(session_id).cloned() else {
        // Core 尚未创建时无法压缩，按忙处理保留标记，ensure 之后再试。
        return ConfigHandoffOutcome::SkippedBusy;
    };
    // core 的手动压缩会等待终态再返回（成功/失败/中断），无需轮询。
    match core.compact_context().await {
        Ok(()) => {
            store.complete_handoff(session_id, target);
            ConfigHandoffOutcome::Aligned
        }
        Err(CoreError::Busy) => ConfigHandoffOutcome::SkippedBusy,
        Err(error) => ConfigHandoffOutcome::Failed(format!("交接压缩未完成：{error}")),
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

/// 纯函数与状态记账单测。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 顺序与重复不影响指纹() {
        let base = execution_fingerprint(&[
            ("fs".into(), "0.1.9".into()),
            ("terminal".into(), "0.3.8".into()),
        ]);
        let reordered = execution_fingerprint(&[
            ("terminal".into(), "0.3.8".into()),
            ("fs".into(), "0.1.9".into()),
            ("fs".into(), "0.1.9".into()),
        ]);
        assert_eq!(base, reordered, "加载顺序与重复项不应触发交接");
    }

    #[test]
    fn 插件版本与集合变化改变指纹() {
        let base = execution_fingerprint(&[("fs".into(), "0.1.9".into())]);
        assert_ne!(
            execution_fingerprint(&[("fs".into(), "0.2.0".into())]),
            base,
            "插件升级即使工具名不变也应触发交接"
        );
        assert_ne!(
            execution_fingerprint(&[
                ("fs".into(), "0.1.9".into()),
                ("terminal".into(), "0.3.8".into()),
            ]),
            base,
            "新增启用插件应触发交接"
        );
        assert_ne!(execution_fingerprint(&[]), base, "停用全部插件应触发交接");
    }

    #[test]
    fn 相邻插件键不因拼接歧义碰撞() {
        // 分隔符缺失时 ["ab","c"] 与 ["a","bc"] 会拼成同一串。
        assert_ne!(
            execution_fingerprint(&[("ab".into(), String::new()), ("c".into(), String::new())]),
            execution_fingerprint(&[("a".into(), String::new()), ("bc".into(), String::new())]),
            "不同插件集合不得产生相同指纹"
        );
    }

    #[test]
    fn 标记与定档的状态记账roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigHandoffStore::new(dir.path().to_path_buf());
        assert!(store.bootstrap_needed("s1"), "首检未做过");
        store.mark_checked("s1");
        assert!(!store.bootstrap_needed("s1"));

        store.set_pending_handoff("s1", "fp-1");
        assert_eq!(store.pending_handoff("s1").as_deref(), Some("fp-1"));

        store.complete_handoff("s1", "fp-1");
        assert!(store.pending_handoff("s1").is_none(), "定档后标记清除");
        assert!(store.pinned_matches("s1", "fp-1"));

        // 定档落盘后新实例可读回（重启不丢定档）。
        let reopened = ConfigHandoffStore::new(dir.path().to_path_buf());
        assert!(reopened.pinned_matches("s1", "fp-1"));

        reopened.forget("s1");
        assert!(!reopened.pinned_matches("s1", "fp-1"), "清理后定档移除");
    }

    #[test]
    fn 首检记录与定档共同决定是否需要交接() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigHandoffStore::new(dir.path().to_path_buf());
        // 无定档：首检必然不一致（新会话由 run_handoff 直接定档）。
        assert!(store.bootstrap_needed("s1"));
        assert!(!store.pinned_matches("s1", "fp-1"));

        store.complete_handoff("s1", "fp-1");
        store.mark_checked("s1");
        assert!(
            !store.bootstrap_needed("s1") && store.pinned_matches("s1", "fp-1"),
            "已首检且指纹一致不应再触发交接"
        );

        // 指纹变化后定档不再匹配——投递路径的首检兜底据此发现漏报。
        assert!(!store.pinned_matches("s1", "fp-2"));
    }
}

/// 端到端：真 TiangongCore + wiremock 模型端点，验证标记制的识别处理、
/// 首检兜底与「尽力压缩一次」的成败判定。
#[cfg(test)]
mod handoff_e2e_tests {
    use super::*;
    use std::time::Duration;

    use tiangong_core::agent_input::{AgentInput, AgentInputKind};
    use tiangong_core::config::core::{CoreConfig, CoreConfigProvider};
    use tiangong_core::permission::TrustMode;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_pair(dir: &tempfile::TempDir) -> (ConfigHandoffStore, CoreManager) {
        (
            ConfigHandoffStore::new(dir.path().to_path_buf()),
            CoreManager::new(
                CoreConfigProvider::new(CoreConfig::default()),
                dir.path().to_path_buf(),
            ),
        )
    }

    /// 写入 models.json：会话模型由 CoreManager 从注册表解析（#556 起
    /// 端点不再随 CoreConfig 传入）。
    fn seed_models(dir: &tempfile::TempDir, base_url: &str) {
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
                "test-model": {
                    "provider": "p",
                    "model": "test-model",
                    "capabilities": ["chat"]
                }
            },
            "routing": { "chat": "test-model" }
        });
        std::fs::write(
            dir.path().join("models.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();
    }

    fn trusted_config() -> CoreConfig {
        CoreConfig::builder()
            .with_trust_mode(TrustMode::FullTrust)
            .build()
    }

    /// 预置带两轮问答历史的会话，并定档一个与目标不一致的旧指纹。
    fn seed_session_with_stale_pin(dir: &tempfile::TempDir, store: &ConfigHandoffStore, id: &str) {
        let mut session = Session::new("交接测试");
        session.id = id.to_string();
        session.bind_storage_root(dir.path().to_path_buf());
        for round in [("第一问", "第一答"), ("第二问", "第二答")] {
            session.append_message(MessageRole::User, round.0);
            session.append_message(MessageRole::Assistant, round.1);
        }
        session.try_persist_to_disk().unwrap();
        store.complete_handoff(id, "stale-fingerprint");
    }

    fn completion_reply(content: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": content}}],
            "usage": {"prompt_tokens": 40, "completion_tokens": 8, "total_tokens": 48}
        }))
    }

    async fn make_core(manager: &CoreManager, id: &str) {
        let (stream_tx, _stream_rx) = std::sync::mpsc::channel();
        manager
            .ensure_core(
                id,
                trusted_config(),
                "/tmp".to_string(),
                stream_tx,
                Vec::new,
            )
            .await
            .expect("ensure_core 失败");
    }

    /// 目标指纹（插件集合变化后的新值）。
    fn target_fingerprint() -> String {
        execution_fingerprint(&[("fs".into(), "0.2.0".into())])
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 首检兜底发现插件变化并经手动压缩交接() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n交接摘要"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &store, "e2e-bootstrap");
        seed_models(&dir, &server.uri());
        make_core(&manager, "e2e-bootstrap").await;

        // 首检：定档与当前指纹不一致 → 压缩交接。
        let fp = target_fingerprint();
        let outcome = ensure_before_deliver(&store, &manager, "e2e-bootstrap", || fp.clone()).await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);

        let session = manager.load_session("e2e-bootstrap").unwrap();
        assert_eq!(
            session.context_summary.as_deref(),
            // 压缩器剥离 [[SUMMARY]] 定界标记后落盘摘要正文（core 既有行为）。
            Some("交接摘要"),
            "交接应推进摘要"
        );
        assert!(session.summary_up_to > 0, "摘要边界应推进");
        assert!(
            store.pinned_matches("e2e-bootstrap", &fp),
            "成功后定档新指纹"
        );

        // 第二次调用：已首检且已定档 → 零模型调用。
        let before = server.received_requests().await.unwrap().len();
        let again = ensure_before_deliver(&store, &manager, "e2e-bootstrap", || fp.clone()).await;
        assert_eq!(again, ConfigHandoffOutcome::Aligned);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            before,
            "已对齐的会话不得再次压缩"
        );
        manager.retire_core("e2e-bootstrap", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 压缩失败定档不再重试() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
        // 压缩恒失败：对话继续后 KV 缓存已按当前上下文写入，重压无意义。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("always fail"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &store, "e2e-fail");
        seed_models(&dir, &server.uri());
        make_core(&manager, "e2e-fail").await;

        let fp = target_fingerprint();
        let outcome = run_handoff(&store, &manager, "e2e-fail", &fp).await;
        assert!(
            matches!(outcome, ConfigHandoffOutcome::Failed(_)),
            "压缩失败应如实返回 Failed"
        );
        assert!(
            store.pending_handoff("e2e-fail").is_none(),
            "失败后不得保留标记（否则每条消息都会重试一轮压缩）"
        );
        assert!(
            store.pinned_matches("e2e-fail", &fp),
            "失败同样定档：新插件集合已接管，历史未经整理"
        );

        // 再次经投递路径检查：不应产生新的压缩请求。
        let before = server.received_requests().await.unwrap().len();
        store.mark_checked("e2e-fail");
        let again = ensure_before_deliver(&store, &manager, "e2e-fail", || fp.clone()).await;
        assert_eq!(again, ConfigHandoffOutcome::Aligned);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            before,
            "失败定档后不得重复压缩"
        );
        manager.retire_core("e2e-fail", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 会话忙时保留标记等下一条消息() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
        // 响应挂起，让会话保持忙态。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("慢回复").set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &store, "e2e-busy");
        seed_models(&dir, &server.uri());
        make_core(&manager, "e2e-busy").await;

        // 起一轮用户对话占住 Core。
        {
            let registry = manager.registry();
            let core = registry.get("e2e-busy").cloned().expect("core");
            drop(registry);
            core.deliver(AgentInputKind::prepared_with_id(
                "m1".to_string(),
                vec![tiangong_types::ContentBlock::text("占用")],
            ))
            .expect("投递失败");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;

        let fp = target_fingerprint();
        let outcome = run_handoff(&store, &manager, "e2e-busy", &fp).await;
        assert_eq!(
            outcome,
            ConfigHandoffOutcome::SkippedBusy,
            "执行中的会话不打断"
        );
        assert_eq!(
            store.pending_handoff("e2e-busy").as_deref(),
            Some(fp.as_str()),
            "忙时保留标记，下一条消息的空闲窗口再做"
        );
        assert!(
            !store.pinned_matches("e2e-busy", &fp),
            "忙不等于已交接，不得定档"
        );
        manager.retire_core("e2e-busy", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 无可交接历史时直接定档零模型调用() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
        let server = MockServer::start().await;
        // 只有 System/Notice 的会话：没有按旧插件集合积累的对话历史。
        let mut session = Session::new("空会话");
        session.id = "e2e-empty".to_string();
        session.bind_storage_root(dir.path().to_path_buf());
        session.append_message(MessageRole::Notice, "系统通知");
        session.try_persist_to_disk().unwrap();
        seed_models(&dir, &server.uri());
        make_core(&manager, "e2e-empty").await;

        let fp = target_fingerprint();
        let outcome = run_handoff(&store, &manager, "e2e-empty", &fp).await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "无可交接历史不得调用模型"
        );
        assert!(store.pinned_matches("e2e-empty", &fp), "直接定档");
        manager.retire_core("e2e-empty", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 变化点打标对活跃会话生效() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n打标摘要"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &store, "e2e-mark");
        seed_models(&dir, &server.uri());
        make_core(&manager, "e2e-mark").await;

        // 插件变化点：给全部活跃会话打标并后台立即处理。
        let fp = target_fingerprint();
        mark_all_sessions(&store, &manager, &fp);
        // 后台任务完成压缩后定档。
        for _ in 0..40 {
            if store.pinned_matches("e2e-mark", &fp) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            store.pinned_matches("e2e-mark", &fp),
            "空闲会话应在变化点当场完成交接"
        );
        assert!(
            store.pending_handoff("e2e-mark").is_none(),
            "完成后清除标记"
        );
        manager.retire_core("e2e-mark", true).await.unwrap();
    }
}
