//! 配置变化上下文交接：宿主层在变化点打标，识别标记后经 core 既有的
//! 手动压缩能力整理上下文，再让新配置接管。
//!
//! 状态记账与编排都在本模块（app 层）——manager 恪守记账定位（Core
//! 注册表/会话文件/创建锁，issue #245），core 只保留通用压缩能力（手动
//! 压缩命令），两者都不感知「交接」概念。runtime（plugin-runtime）同样
//! 零改动：变化感知在 app 即全覆盖（app 依赖 runtime，插件命令共用
//! `notify_plugins_changed` 尾部、模型切换在 `sync_core_config_from_state`）。
//!
//! 运行期是**标记制**：变化发生的地方给活跃会话打标并立即尝试处理
//! （空闲即压，忙则留标记）；投递消息路径只查内存标记。兜底是**首检
//! 指纹比对**：每会话每进程首次投递前算一次执行配置指纹与落盘定档
//! 比对，防两类漏报——进程重启丢内存标记、未走正常变化入口的变化。
//! 定档指纹持久化在 storage_root 下（不进会话文件）——会话复制/导出后
//! 丢失定档只会重新定档，不触发无谓交接。
//!
//! 指纹输入全部是「声明态」：模型（协议+端点+模型名，凭据绝不参与）与
//! 插件（id@version + 启用集合）。插件部分覆盖注册在 plugin-runtime 的
//! 全部已安装插件（含 prompt 文案插件）——它们都是动态注册的，装卸/
//! 升级/启停一律经 registry，天然全部参与指纹；编译期编进 App 的进程内
//! 插件是历史形态（当前已不存在，runtime 保留该形态仅作动态插件载体），
//! 即便未来恢复，其恒定性质也无需参与指纹。加载顺序、运行期抖动（如
//! sidecar 临时掉线）不改变指纹；反之插件升级即使工具名不变也会触发
//! 交接——历史里按旧版本产生的调用与新版本行为可能已不一致。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tiangong_core::agent_input::{AgentInput, AgentInputKind, CommandInput};
use tiangong_core::core::CoreError;
use tiangong_core::session::{MessageRole, Session};
use tiangong_core_manager::CoreManager;
use tiangong_llm::ModelEndpoint;

/// 定档指纹存储文件（storage_root 下，session_id → 指纹摘要）。
const FINGERPRINTS_FILE: &str = "config-fingerprints.json";
/// 等待交接压缩收敛的上限：大历史会话的摘要输出可能需要数分钟；超时
/// 后消息照常投递（进行中的压缩会被用户消息按既有语义取消并起新轮）。
const HANDOFF_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(180);
/// 等待收敛的轮询间隔。
const HANDOFF_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// 一次交接检查的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigHandoffOutcome {
    /// 配置已对齐：指纹一致、首次定档、无可交接历史或交接完成。
    Aligned,
    /// 会话正在执行 turn：不打断，消息照常投递，标记保留待下一条消息。
    SkippedBusy,
    /// 交接压缩未完成（模型错误/超时）：上下文未整理即继续，标记保留重试。
    Failed(String),
}

/// 交接内存状态：待处理标记 + 首检记录 + 定档指纹缓存（懒加载）。
#[derive(Default)]
struct HandoffState {
    /// 待交接标记：session_id → 目标指纹（变化点写入，交接完成清除、
    /// 失败/忙保留供投递路径重试）。
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

    /// 写入待交接标记（变化点与交接失败时调用；幂等，后写覆盖目标指纹）。
    fn set_pending_handoff(&self, session_ids: &[String], target_fingerprint: &str) {
        if session_ids.is_empty() {
            return;
        }
        self.lock_state().pending.extend(
            session_ids
                .iter()
                .map(|id| (id.clone(), target_fingerprint.to_string())),
        );
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

    /// 交接完成：定档目标指纹（落盘）并清除待交接标记。
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

/// 计算执行配置指纹：模型标识 + 启用的 runtime 注册插件（id@version）。
///
/// 插件输入应是 plugin-runtime 注册表启用集合的声明态——注册在 runtime
/// 的插件都是动态注册的（含 prompt），装卸/升级/启停全部经 registry 变化，
/// 因此无需任何特判。插件键排序去重后参与摘要（加载顺序不是能力变化）；
/// 无版本的插件只记 id。凭据（api_key/headers）与运行参数
/// （trust_mode/reasoning_effort）不参与：前者绝不落盘，后者下一轮直接
/// 生效、无需交接。
pub(crate) fn execution_fingerprint(
    endpoint: &ModelEndpoint,
    plugins: &[(String, String)],
) -> String {
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
    hasher.update(
        format!(
            "{:?}|{}|{}",
            endpoint.protocol, endpoint.base_url, endpoint.model
        )
        .as_bytes(),
    );
    for key in plugin_keys {
        hasher.update(b"\x1f");
        hasher.update(key.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 变化点入口（会话显式切换模型 / 会话实际执行端点变化）：单会话打标
/// 并立即尝试处理。空闲当场压缩（压缩用 Core 当前配置——切换场景下
/// 即旧模型，天然实现「原模型优先」），忙则留标记等投递路径。
pub(crate) fn mark_session(
    store: &ConfigHandoffStore,
    manager: &CoreManager,
    session_id: &str,
    target_fingerprint: &str,
) {
    store.set_pending_handoff(
        std::slice::from_ref(&session_id.to_string()),
        target_fingerprint,
    );
    // 有 runtime 上下文（含 tauri async runtime 线程）时立即处理；
    // 否则只留标记，由投递路径处理。
    let target = target_fingerprint.to_string();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let store = store.clone();
        let manager = manager.clone();
        let id = session_id.to_string();
        handle.spawn(async move {
            if let ConfigHandoffOutcome::Failed(reason) =
                run_handoff(&store, &manager, &id, &target).await
            {
                tracing::warn!(session_id = %id, reason, "配置交接压缩未完成，标记保留重试");
            }
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

/// 是否存在待交接标记（投递前快速判定，供延迟投递决策）。
pub(crate) fn has_pending(store: &ConfigHandoffStore, session_id: &str) -> bool {
    store.pending_handoff(session_id).is_some()
}

/// 首检是否会发现指纹不一致（有标记、或未做过首检且当前指纹与定档
/// 不同）。只读判定，不写首检记录——真正的首检在随后的交接序列执行。
pub(crate) fn bootstrap_mismatch<F>(
    store: &ConfigHandoffStore,
    session_id: &str,
    fingerprint: F,
) -> bool
where
    F: FnOnce() -> String,
{
    if store.pending_handoff(session_id).is_some() {
        return true;
    }
    if !store.bootstrap_needed(session_id) {
        return false;
    }
    let current = fingerprint();
    !store.pinned_matches(session_id, &current)
}

/// 直接种档（不经压缩）：定档目标指纹并清除待交接标记。供「首次设置
/// 模型不视为切换」等无需整理的路径收尾，保证首检不再误判。
pub(crate) fn finalize_handoff(store: &ConfigHandoffStore, session_id: &str, fingerprint: &str) {
    store.complete_handoff(session_id, fingerprint);
}

/// 面向显式切换的幂等交接入口：目标指纹已定档（或已有标记在途）时按
/// 标记语义处理，否则执行交接——重复设置同一模型不会引发无谓压缩。
pub(crate) async fn handoff_to(
    store: &ConfigHandoffStore,
    manager: &CoreManager,
    session_id: &str,
    target: &str,
) -> ConfigHandoffOutcome {
    if store.pending_handoff(session_id).is_some() {
        return run_handoff(store, manager, session_id, target).await;
    }
    if store.pinned_matches(session_id, target) {
        return ConfigHandoffOutcome::Aligned;
    }
    run_handoff(store, manager, session_id, target).await
}

/// 执行一次交接：无可交接历史直接定档；否则注入手动压缩并等待收敛，
/// 摘要边界推进才算成功并定档。失败/忙写入待交接标记供投递路径重试。
pub(crate) async fn run_handoff(
    store: &ConfigHandoffStore,
    manager: &CoreManager,
    session_id: &str,
    target: &str,
) -> ConfigHandoffOutcome {
    let outcome = run_handoff_inner(store, manager, session_id, target).await;
    if !matches!(outcome, ConfigHandoffOutcome::Aligned) {
        // 未完成：写入/保留待交接标记——变化点路径本就有标记（幂等），
        // 首检兜底路径据此获得后续投递前的重试入口。
        store.set_pending_handoff(std::slice::from_ref(&session_id.to_string()), target);
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
            store.complete_handoff(session_id, target);
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

/// 纯函数与状态记账单测。
#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(model: &str) -> ModelEndpoint {
        ModelEndpoint {
            model: model.to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key: "sk-secret".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn 凭据与顺序不参与指纹() {
        let mut other_key = endpoint("gpt-4o");
        other_key.api_key = "sk-another".to_string();
        let base = execution_fingerprint(
            &endpoint("gpt-4o"),
            &[
                ("fs".into(), "0.1.9".into()),
                ("terminal".into(), "0.3.8".into()),
            ],
        );
        let same = execution_fingerprint(
            &other_key,
            &[
                ("terminal".into(), "0.3.8".into()),
                ("fs".into(), "0.1.9".into()),
            ],
        );
        assert_eq!(base, same, "换凭据/调序不应触发交接");
        assert!(!base.contains("sk-"), "摘要不得包含凭据原文");
    }

    #[test]
    fn 模型与插件版本变化改变指纹() {
        let plugins = [("fs".to_string(), "0.1.9".to_string())];
        let base = execution_fingerprint(&endpoint("gpt-4o"), &plugins);
        assert_ne!(
            execution_fingerprint(&endpoint("claude"), &plugins),
            base,
            "换模型应触发交接"
        );
        assert_ne!(
            execution_fingerprint(&endpoint("gpt-4o"), &[("fs".into(), "0.2.0".into())]),
            base,
            "插件升级即使工具名不变也应触发交接"
        );
    }

    #[test]
    fn 标记与定档的状态记账roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigHandoffStore::new(dir.path().to_path_buf());

        // 待交接标记：写入、读取、完成清除。
        assert_eq!(store.pending_handoff("s1"), None);
        store.set_pending_handoff(&["s1".to_string()], "fp-target");
        assert_eq!(store.pending_handoff("s1").as_deref(), Some("fp-target"));
        store.set_pending_handoff(&["s1".to_string()], "fp-newer");
        assert_eq!(
            store.pending_handoff("s1").as_deref(),
            Some("fp-newer"),
            "变化点再次打标覆盖目标"
        );
        store.complete_handoff("s1", "fp-newer");
        assert_eq!(store.pending_handoff("s1"), None, "完成清除标记");
        assert!(store.pinned_matches("s1", "fp-newer"));

        // 首检记录：一次性。
        assert!(store.bootstrap_needed("s2"));
        store.mark_checked("s2");
        assert!(!store.bootstrap_needed("s2"));

        // 定档持久化：新实例懒加载恢复；首检记录仅进程内，重启后重做。
        let restored = ConfigHandoffStore::new(dir.path().to_path_buf());
        assert!(restored.pinned_matches("s1", "fp-newer"));
        assert!(
            restored.bootstrap_needed("s2"),
            "首检记录不落盘，新进程重做首检兜底"
        );
        restored.forget("s1");
        assert!(!restored.pinned_matches("s1", "fp-newer"));
        let again = ConfigHandoffStore::new(dir.path().to_path_buf());
        assert!(!again.pinned_matches("s1", "fp-newer"), "遗忘须落盘");
    }
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

    fn make_pair(dir: &tempfile::TempDir) -> (ConfigHandoffStore, CoreManager) {
        (
            ConfigHandoffStore::new(dir.path().to_path_buf()),
            CoreManager::new(
                CoreConfigProvider::new(CoreConfig::default()),
                dir.path().to_path_buf(),
            ),
        )
    }

    fn config_for(base_url: &str) -> CoreConfig {
        CoreConfig::builder()
            .with_chat(base_url, "test-key", "test-model")
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
        execution_fingerprint(
            &ModelEndpoint {
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
        let (store, manager) = make_pair(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n交接摘要"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &store, "e2e-bootstrap");
        make_core(&manager, "e2e-bootstrap", &server.uri()).await;

        // 首检：定档与当前指纹不一致 → 压缩交接。
        let fp = fingerprint_for(&server.uri());
        let outcome = ensure_before_deliver(&store, &manager, "e2e-bootstrap", || fp.clone()).await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);

        let session = manager.load_session("e2e-bootstrap").unwrap();
        assert_eq!(
            session.context_summary.as_deref(),
            // 压缩器剥离 [[SUMMARY]] 定界标记后落盘摘要正文（core 既有行为）。
            Some("交接摘要")
        );
        assert!(session.summary_up_to > 0, "摘要边界应推进");
        assert!(
            store.pinned_matches("e2e-bootstrap", &fp),
            "交接成功后定档须更新"
        );
        // 之后的投递路径：首检已做、无标记，惰性指纹不再求值。
        let mut evaluated = false;
        let outcome = ensure_before_deliver(&store, &manager, "e2e-bootstrap", || {
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
        let (store, manager) = make_pair(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("compression unavailable"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &store, "e2e-retry");
        make_core(&manager, "e2e-retry", &server.uri()).await;

        let fp = fingerprint_for(&server.uri());
        let outcome = ensure_before_deliver(&store, &manager, "e2e-retry", || fp.clone()).await;
        assert!(matches!(outcome, ConfigHandoffOutcome::Failed(_)));
        assert!(
            store.pending_handoff("e2e-retry").is_some(),
            "失败必须写入待交接标记，投递路径据此重试"
        );
        assert!(!store.pinned_matches("e2e-retry", &fp), "失败须保留旧定档");

        // 模型恢复后（新端点+变化点刷新目标），后台处理完成交接。
        drop(server);
        manager.retire_core("e2e-retry", false).await.unwrap();
        let recovered = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n重试摘要"))
            .mount(&recovered)
            .await;
        make_core(&manager, "e2e-retry", &recovered.uri()).await;
        let new_target = fingerprint_for(&recovered.uri());
        mark_session(&store, &manager, "e2e-retry", &new_target);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !store.pinned_matches("e2e-retry", &new_target) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "变化点后台处理应及时完成交接"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(store.pending_handoff("e2e-retry"), None);
        manager.retire_core("e2e-retry", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 会话执行中不打断并跳过交接() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
        let server = MockServer::start().await;
        // 压缩响应长时间挂起：占住任务槽，构造「会话忙」。
        Mock::given(method("POST"))
            .respond_with(completion_reply("慢摘要").set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &store, "e2e-busy");
        make_core(&manager, "e2e-busy", &server.uri()).await;

        // 变化点打标（写标记 + spawn 后台处理）：后台交接的压缩挂起中。
        let fp = fingerprint_for(&server.uri());
        mark_session(&store, &manager, "e2e-busy", &fp);
        tokio::time::sleep(Duration::from_millis(400)).await;
        // 压缩进行中标记在场；投递路径执行：deliver 得 Busy，不打断。
        assert!(store.pending_handoff("e2e-busy").is_some());
        let second = ensure_before_deliver(&store, &manager, "e2e-busy", || fp.clone()).await;
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
            store.pending_handoff("e2e-busy").is_some(),
            "失败保留标记，投递路径据此重试"
        );
        manager.retire_core("e2e-busy", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 变化点打标后空闲会话立即交接() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n识别即压摘要"))
            .mount(&server)
            .await;
        // 会话在当前指纹下已定档（模拟上一进程的正常收尾）。
        seed_session_with_stale_pin(&dir, &store, "e2e-mark");
        make_core(&manager, "e2e-mark", &server.uri()).await;
        store.complete_handoff("e2e-mark", &fingerprint_for(&server.uri()));

        // 变化点打标（目标=新配置指纹），后台立即处理空闲会话。
        let target = fingerprint_for("https://changed.example.com");
        mark_session(&store, &manager, "e2e-mark", &target);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !store.pinned_matches("e2e-mark", &target) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "打标后空闲会话应及时完成交接"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(store.pending_handoff("e2e-mark"), None, "完成后清除标记");
        let session = manager.load_session("e2e-mark").unwrap();
        assert_eq!(session.context_summary.as_deref(), Some("识别即压摘要"));
        manager.retire_core("e2e-mark", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 压缩失败仍完成切换且投递固化实际模型() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
        // 压缩恒失败（500）的模型端点：对话一旦跑起来重试压缩无意义，
        // 切换必达（历史未经整理仅告警）。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("always fail"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &store, "e2e-persist");
        make_core(&manager, "e2e-persist", &server.uri()).await;
        let state = std::sync::Arc::new(tokio::sync::Mutex::new(
            tiangong_app_state::app_state::TiangongState::new(),
        ));
        let new_endpoint = tiangong_llm::ModelEndpoint {
            base_url: "https://changed.example.com".into(),
            model: "m".into(),
            api_key: "k".into(),
            ..Default::default()
        };

        // 压缩失败（Failed）后仍完成切换——不再返回 Err。
        let outcome = crate::app::apply_session_model_at(
            &store,
            &manager,
            std::path::Path::new("/nonexistent"),
            "e2e-persist",
            Some("new-key".to_string()),
            new_endpoint,
        )
        .await;
        assert!(outcome.is_ok(), "压缩失败不得阻断切换");
        let current = {
            let registry = manager.registry();
            registry
                .get("e2e-persist")
                .map(|core| core.current_endpoint())
        };
        assert_eq!(
            current.expect("core").base_url,
            "https://changed.example.com",
            "失败后端点仍应切换生效"
        );

        // 投递固化：会话引用已被切换收尾写为 new-key（端点不在测试注册
        // 表，反查无匹配）——固化保持现值不变。
        crate::app::pin_effective_model(&state, &manager, "e2e-persist").await;
        let session = manager.load_session("e2e-persist").unwrap();
        assert_eq!(
            session.model_ref,
            Some("new-key".to_string()),
            "端点不在注册表时固化保持现值"
        );
        manager.retire_core("e2e-persist", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 首次设置模型不压缩直接切换() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
        let server = MockServer::start().await;
        // 老会话：从未设置过 model_ref（升级兼容场景），Core 也从未切换
        // 过端点；用户首次指定模型 k。
        let mut session = Session::new("老会话");
        session.id = "e2e-first".to_string();
        session.bind_storage_root(dir.path().to_path_buf());
        for round in [("第一问", "第一答"), ("第二问", "第二答")] {
            session.append_message(MessageRole::User, round.0);
            session.append_message(MessageRole::Assistant, round.1);
        }
        session.model_ref = Some("k".to_string());
        session.try_persist_to_disk().unwrap();

        let state = std::sync::Arc::new(tokio::sync::Mutex::new(
            tiangong_app_state::app_state::TiangongState::new(),
        ));
        // 注册表提供模型 k（独立端点）。
        {
            let mut guard = state.lock().await;
            guard.config.models.providers.insert(
                "p".to_string(),
                tiangong_llm::models_config::ProviderConfig {
                    headers: Default::default(),
                    base_url: "https://k.example.com".to_string(),
                    api_key: "k".to_string(),
                    timeout_ms: 60_000,
                    protocol: tiangong_llm::model::ProviderProtocol::OpenAiChatCompletions,
                },
            );
            guard.config.models.models.insert(
                "k".to_string(),
                tiangong_llm::models_config::ModelEntry {
                    provider: "p".to_string(),
                    model: "k-model".to_string(),
                    capabilities: vec![tiangong_llm::models_config::ModelCapability::Chat],
                    ..Default::default()
                },
            );
        }
        make_core(&manager, "e2e-first", &server.uri()).await;

        // 端点漂移（k ≠ 当前默认）→ 首次设置：不压缩直接切换。
        let outcome = crate::app::reconcile_session_endpoint_at(
            &state,
            &store,
            &manager,
            std::path::Path::new("/nonexistent"),
            "e2e-first",
        )
        .await;
        assert!(outcome.is_ok());
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "首次设置模型不得触发压缩调用"
        );
        let current = {
            let registry = manager.registry();
            registry
                .get("e2e-first")
                .map(|core| core.current_endpoint())
        };
        assert_eq!(
            current.expect("core").base_url,
            "https://k.example.com",
            "首次设置直接切换端点"
        );
        assert_eq!(
            manager.load_session("e2e-first").unwrap().model_ref,
            Some("k".to_string()),
            "引用保持用户设定"
        );
        manager.retire_core("e2e-first", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 无可交接历史时直接定档零模型调用() {
        let dir = tempfile::tempdir().unwrap();
        let (store, manager) = make_pair(&dir);
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
        let outcome = ensure_before_deliver(&store, &manager, "system-only", || fp.clone()).await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);
        assert!(store.pinned_matches("system-only", &fp));
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "无可交接历史不得发起模型调用"
        );

        // 会话文件不存在（新对话）同样直接定档。
        let outcome = ensure_before_deliver(&store, &manager, "fresh", || fp.clone()).await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);
        assert!(store.pinned_matches("fresh", &fp));
    }
}
