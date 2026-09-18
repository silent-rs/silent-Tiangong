//! 配置变化上下文交接：宿主层在变化点打标，manager 识别标记后经 core
//! 既有的手动压缩能力整理上下文，再让新配置接管。
//!
//! 决策归宿宿主层（与 issue #245 的分层一致）：core 只保留通用压缩执行
//! 能力（`Command::CompressContext`），「何时压缩、为什么压缩」在这里。
//! 运行期是**标记制**——变化发生的地方（插件装卸/升级/启停、模型切换）
//! 调 [`CoreManager::mark_sessions_for_handoff`] 给活跃会话打标并立即尝试
//! 处理（空闲即压，忙则留标记）；投递消息路径只查内存标记，零指纹计算。
//!
//! 兜底是**首检指纹比对**：每会话每进程首次投递前算一次执行配置指纹
//! （模型标识 + 启用插件 id@version）与落盘定档比对，防两类漏报——进程
//! 重启丢内存标记（插件升级+重启激活是常见组合）、未走正常变化入口的
//! 变化。定档指纹持久化在宿主侧（`config-fingerprints.json`），不进会话
//! 文件——会话复制/导出后丢失定档只会重新定档，不触发无谓交接。
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

use tiangong_core::agent_input::{AgentInput, AgentInputKind, CommandInput};
use tiangong_core::core::CoreError;
use tiangong_core::session::{MessageRole, Session};
use tiangong_llm::ModelEndpoint;

use crate::CoreManager;

/// 定档指纹存储文件（storage_root 下，session_id → 指纹摘要）。
const FINGERPRINTS_FILE: &str = "config-fingerprints.json";
/// 等待交接压缩收敛的上限：压缩调用通常数秒到数十秒完成；超时后消息
/// 照常投递（进行中的压缩会被用户消息按既有语义取消并起新轮）。
const HANDOFF_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);
/// 等待收敛的轮询间隔。
const HANDOFF_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// 一次交接检查的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigHandoffOutcome {
    /// 配置已对齐：指纹一致、首次定档、无可交接历史或交接完成。
    Aligned,
    /// 会话正在执行 turn：不打断，消息照常投递，标记保留待下一条消息。
    SkippedBusy,
    /// 交接压缩未完成（模型错误/超时）：上下文未整理即继续，标记保留重试。
    Failed(String),
}

/// 交接内存状态：待处理标记 + 首检记录 + 定档指纹缓存（懒加载）。
#[derive(Default)]
pub(crate) struct HandoffState {
    /// 待交接标记：session_id → 目标指纹（变化点写入，交接完成清除）。
    pending: HashMap<String, String>,
    /// 本进程已完成首检指纹兜底比对的会话（首检每会话每进程一次）。
    checked: HashSet<String>,
    /// 定档指纹表（session_id → 摘要），None 表示尚未从磁盘加载。
    pinned: Option<HashMap<String, String>>,
}

/// 计算执行配置指纹：模型标识 + 启用的 runtime 注册插件（id@version）。
///
/// 插件输入应是 plugin-runtime 注册表启用集合的声明态——注册在 runtime
/// 的插件都是动态注册的（含 prompt），装卸/升级/启停全部经 registry 变化，
/// 因此无需任何特判。插件键排序去重后参与摘要（加载顺序不是能力变化）；
/// 无版本的插件只记 id。凭据（api_key/headers）与运行参数
/// （trust_mode/reasoning_effort）不参与：前者绝不落盘，后者下一轮直接
/// 生效、无需交接。
pub fn execution_fingerprint(endpoint: &ModelEndpoint, plugins: &[(String, String)]) -> String {
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

impl CoreManager {
    // ===== 变化点：打标 + 立即处理 =====

    /// 变化点入口（插件装卸/升级/启停、模型切换）：给全部活跃会话打上
    /// 待交接标记，并逐会话立即尝试处理——空闲会话当场压缩，忙会话留
    /// 标记等投递路径处理。处理在后台并行进行，本方法打标后即返回。
    ///
    /// 不活跃会话（无 Core）不打标：其配置变化由首检兜底发现。
    pub fn mark_sessions_for_handoff(&self, target_fingerprint: &str) {
        let session_ids: Vec<String> = {
            let registry = self.registry();
            let ids: Vec<String> = registry.iter().map(|(id, _)| id.clone()).collect();
            drop(registry);
            let mut state = self.lock_handoff_state();
            for id in &ids {
                state
                    .pending
                    .insert(id.clone(), target_fingerprint.to_string());
            }
            ids
        };
        tracing::info!(
            sessions = session_ids.len(),
            "执行配置已变化，活跃会话标记待交接并开始处理"
        );
        // 有 runtime 上下文（含 tauri async runtime 线程）时立即并行处理；
        // 否则只留标记，由投递路径处理。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            for id in session_ids {
                let manager = self.clone();
                handle.spawn(async move {
                    if let ConfigHandoffOutcome::Failed(reason) =
                        manager.run_pending_handoff(&id).await
                    {
                        tracing::warn!(session_id = %id, reason, "配置交接压缩未完成，标记保留重试");
                    }
                });
            }
        }
    }

    // ===== 投递路径 =====

    /// 是否存在待交接标记（纯内存快查，投递路径的高频入口）。
    pub fn pending_handoff(&self, session_id: &str) -> bool {
        self.lock_handoff_state().pending.contains_key(session_id)
    }

    /// 本会话是否尚未做过首检指纹兜底比对（每会话每进程一次）。
    pub fn handoff_bootstrap_needed(&self, session_id: &str) -> bool {
        !self.lock_handoff_state().checked.contains(session_id)
    }

    /// 执行待交接标记（投递路径：快查命中时调用）。
    pub async fn run_pending_handoff(&self, session_id: &str) -> ConfigHandoffOutcome {
        let Some(target) = self.lock_handoff_state().pending.get(session_id).cloned() else {
            return ConfigHandoffOutcome::Aligned;
        };
        self.perform_handoff(session_id, &target).await
    }

    /// 首检兜底（每会话每进程首次投递前）：与落盘定档比对指纹，不一致时
    /// 执行交接。失败写入待交接标记，使后续投递路径继续重试——重启丢
    /// 内存标记、未走正常变化入口的变化都在这里被发现。
    pub async fn bootstrap_handoff_check(
        &self,
        session_id: &str,
        current_fingerprint: &str,
    ) -> ConfigHandoffOutcome {
        if self.pending_handoff(session_id) {
            return self.run_pending_handoff(session_id).await;
        }
        self.lock_handoff_state()
            .checked
            .insert(session_id.to_string());
        if self.pinned_fingerprint_matches(session_id, current_fingerprint) {
            return ConfigHandoffOutcome::Aligned;
        }
        self.perform_handoff(session_id, current_fingerprint).await
    }

    // ===== 交接执行核心 =====

    /// 执行一次交接：无可交接历史直接定档；否则注入手动压缩并等待收敛，
    /// 摘要边界推进才算成功并定档。失败/忙保留待交接标记。
    async fn perform_handoff(&self, session_id: &str, target: &str) -> ConfigHandoffOutcome {
        let outcome = self.perform_handoff_inner(session_id, target).await;
        if !matches!(outcome, ConfigHandoffOutcome::Aligned) {
            // 未完成：写入/保留待交接标记——变化点路径本就有标记（幂等），
            // 首检兜底路径据此获得后续投递前的重试入口。
            self.lock_handoff_state()
                .pending
                .insert(session_id.to_string(), target.to_string());
        }
        outcome
    }

    async fn perform_handoff_inner(&self, session_id: &str, target: &str) -> ConfigHandoffOutcome {
        let session = match self.load_session(session_id) {
            Ok(session) => session,
            Err(_) => {
                // 会话文件尚不存在（新对话）：没有按旧配置积累的上下文。
                self.complete_handoff(session_id, target);
                return ConfigHandoffOutcome::Aligned;
            }
        };
        if !has_handable_history(&session) {
            self.complete_handoff(session_id, target);
            return ConfigHandoffOutcome::Aligned;
        }
        let Some(core) = self.registry().get(session_id).cloned() else {
            // 标记保留：Core 尚未创建时无法压缩，ensure 之后投递路径会再处理。
            return ConfigHandoffOutcome::Failed("会话无活跃 Core".to_string());
        };
        let summary_before = session.summary_up_to;
        match core.deliver(AgentInputKind::Command(CommandInput::CompressContext)) {
            Ok(()) => {}
            Err(CoreError::Busy) => return ConfigHandoffOutcome::SkippedBusy,
            Err(error) => {
                return ConfigHandoffOutcome::Failed(format!("交接压缩注入失败：{error}"));
            }
        }
        // deliver 返回 Ok 时压缩任务已注册任务槽，is_busy 立即可靠。
        let deadline = tokio::time::Instant::now() + HANDOFF_WAIT_LIMIT;
        while core.is_busy() {
            if tokio::time::Instant::now() >= deadline {
                return ConfigHandoffOutcome::Failed("交接压缩等待超时".to_string());
            }
            tokio::time::sleep(HANDOFF_POLL_INTERVAL).await;
        }
        match self.load_session(session_id) {
            Ok(after) if after.summary_up_to > summary_before => {
                self.complete_handoff(session_id, target);
                ConfigHandoffOutcome::Aligned
            }
            Ok(_) => ConfigHandoffOutcome::Failed(
                "交接压缩未推进摘要边界（模型失败或无可压缩内容）".to_string(),
            ),
            Err(error) => ConfigHandoffOutcome::Failed(format!("压缩后回读会话失败：{error}")),
        }
    }

    /// 交接完成：定档目标指纹（落盘）并清除待交接标记。
    fn complete_handoff(&self, session_id: &str, fingerprint: &str) {
        let changed = {
            let mut state = self.lock_handoff_state();
            state.pending.remove(session_id);
            let pinned = state.pinned.get_or_insert_with(HashMap::new);
            pinned.insert(session_id.to_string(), fingerprint.to_string())
                != Some(fingerprint.to_string())
        };
        if changed {
            self.save_fingerprints();
        }
    }

    /// 定档指纹是否与给定值一致。
    fn pinned_fingerprint_matches(&self, session_id: &str, fingerprint: &str) -> bool {
        self.lock_handoff_state()
            .pinned
            .as_ref()
            .and_then(|pinned| pinned.get(session_id))
            .is_some_and(|p| p == fingerprint)
    }

    /// 移除会话的交接状态（会话删除/物理清理时调用，避免残留脏记录）。
    pub fn forget_config_fingerprint(&self, session_id: &str) {
        let removed = {
            let mut state = self.lock_handoff_state();
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

    /// 交接状态锁：首次访问时从磁盘懒加载定档表，之后驻留内存。
    fn lock_handoff_state(&self) -> std::sync::MutexGuard<'_, HandoffState> {
        let mut guard = self
            .handoff_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
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
        let snapshot = self.lock_handoff_state().pinned.clone().unwrap_or_default();
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

/// 纯函数与状态机单测；端到端（真 TiangongCore + mock 模型）见下个模块。
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
        assert_eq!(
            execution_fingerprint(&endpoint("gpt-4o"), &[("builtin".into(), String::new())]),
            execution_fingerprint(&endpoint("gpt-4o"), &[("builtin".into(), String::new())]),
            "无版本插件只记 id，不引入无谓变化"
        );
    }

    #[test]
    fn 定档存储roundtrip与遗忘() {
        let dir = tempfile::tempdir().unwrap();
        let manager = crate::core_manager::tests::make_manager(&dir);
        let fp = execution_fingerprint(&endpoint("gpt-4o"), &[]);
        assert!(!manager.pinned_fingerprint_matches("s1", &fp));
        manager.complete_handoff("s1", &fp);
        assert!(manager.pinned_fingerprint_matches("s1", &fp));

        // 新实例从磁盘恢复（懒加载路径）。
        let restored = crate::core_manager::tests::make_manager(&dir);
        assert!(restored.pinned_fingerprint_matches("s1", &fp));
        restored.forget_config_fingerprint("s1");
        assert!(!restored.pinned_fingerprint_matches("s1", &fp));
        let again = crate::core_manager::tests::make_manager(&dir);
        assert!(!again.pinned_fingerprint_matches("s1", &fp), "遗忘须落盘");
    }

    #[tokio::test]
    async fn 标记与首检记录的状态机() {
        let dir = tempfile::tempdir().unwrap();
        let manager = crate::core_manager::tests::make_manager(&dir);
        assert!(manager.handoff_bootstrap_needed("s1"));
        assert!(!manager.pending_handoff("s1"));

        // 打标（无活跃 Core 的会话不在标记范围，由首检兜底）。
        manager.mark_sessions_for_handoff("fp-target");
        assert!(!manager.pending_handoff("s1"));

        // 首检记录写入后不再重复首检；待交接标记命中时首检转交执行。
        let outcome = manager.bootstrap_handoff_check("s1", "fp-current").await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned, "新会话直接定档");
        assert!(!manager.handoff_bootstrap_needed("s1"));
        assert!(!manager.pending_handoff("s1"));
        assert!(manager.pinned_fingerprint_matches("s1", "fp-current"));
    }

    #[tokio::test]
    async fn 无可交接历史时直接定档() {
        let dir = tempfile::tempdir().unwrap();
        let manager = crate::core_manager::tests::make_manager(&dir);
        // 只有 System/Notice（或历史已折叠）的会话直接定档、零模型调用。
        let mut session = Session::new("仅系统消息");
        session.id = "system-only".to_string();
        session.bind_storage_root(dir.path().to_path_buf());
        session.messages.push(tiangong_core::session::Message::new(
            MessageRole::Notice,
            "提示",
        ));
        session.try_persist_to_disk().unwrap();
        let fp = execution_fingerprint(&endpoint("gpt-4o"), &[]);
        let outcome = manager.bootstrap_handoff_check("system-only", &fp).await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);
        assert!(manager.pinned_fingerprint_matches("system-only", &fp));
    }
}

/// 端到端：真 TiangongCore + wiremock 模型端点，验证标记制的识别处理、
/// 首检兜底与成败判定。
#[cfg(test)]
mod handoff_e2e_tests {
    use super::*;
    use std::time::Duration;

    use tiangong_core::config::core::CoreConfig;
    use tiangong_core::permission::TrustMode;
    use tiangong_core::session::MessageRole;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
        let stale = execution_fingerprint(
            &ModelEndpoint {
                model: "old-model".into(),
                base_url: "https://old.example.com".into(),
                api_key: "sk-old".into(),
                ..Default::default()
            },
            &[],
        );
        manager.complete_handoff(id, &stale);
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
        let manager = crate::core_manager::tests::make_manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n交接摘要"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &manager, "e2e-bootstrap");
        make_core(&manager, "e2e-bootstrap", &server.uri()).await;

        // 首检：定档（旧模型）与当前指纹不一致 → 压缩交接。
        let fp = fingerprint_for(&server.uri());
        let outcome = manager.bootstrap_handoff_check("e2e-bootstrap", &fp).await;
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
        // 之后的投递路径：首检已做、无标记，纯内存快查通过。
        assert!(!manager.handoff_bootstrap_needed("e2e-bootstrap"));
        assert!(!manager.pending_handoff("e2e-bootstrap"));
        assert_eq!(requests_of(&server).await, 1, "交接只应产生一次压缩请求");
        manager.retire_core("e2e-bootstrap", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 首检压缩失败写入标记供投递路径重试() {
        let dir = tempfile::tempdir().unwrap();
        let manager = crate::core_manager::tests::make_manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("compression unavailable"))
            .mount(&server)
            .await;
        seed_session_with_stale_pin(&dir, &manager, "e2e-retry");
        make_core(&manager, "e2e-retry", &server.uri()).await;

        let fp = fingerprint_for(&server.uri());
        let outcome = manager.bootstrap_handoff_check("e2e-retry", &fp).await;
        assert!(matches!(outcome, ConfigHandoffOutcome::Failed(_)));
        assert!(
            manager.pending_handoff("e2e-retry"),
            "首检失败必须写入待交接标记，投递路径据此重试"
        );
        assert!(
            !manager.pinned_fingerprint_matches("e2e-retry", &fp),
            "失败须保留旧定档"
        );

        // 模型恢复后，投递路径经标记重试成功。
        let recovered = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n重试摘要"))
            .mount(&recovered)
            .await;
        // 模型恢复后重建 Core 指向新端点；生产中配置变化必经变化点打标
        // 刷新目标指纹，重试按最新目标定档。
        drop(server);
        manager.retire_core("e2e-retry", false).await.unwrap();
        make_core(&manager, "e2e-retry", &recovered.uri()).await;
        let new_target = fingerprint_for(&recovered.uri());
        manager.mark_sessions_for_handoff(&new_target);
        // mark 已 spawn 后台处理，走投递路径的显式重试验证标记语义。
        let outcome = manager.run_pending_handoff("e2e-retry").await;
        assert_eq!(outcome, ConfigHandoffOutcome::Aligned);
        assert!(
            manager.pinned_fingerprint_matches("e2e-retry", &new_target),
            "重试成功后按最新目标定档"
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while manager.pending_handoff("e2e-retry") {
            assert!(
                tokio::time::Instant::now() < deadline,
                "后台处理应及时清除标记"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        manager.retire_core("e2e-retry", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 会话执行中不打断并跳过交接() {
        let dir = tempfile::tempdir().unwrap();
        let manager = crate::core_manager::tests::make_manager(&dir);
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
        manager.mark_sessions_for_handoff(&fp);
        tokio::time::sleep(Duration::from_millis(400)).await;
        // 压缩进行中标记在场；第二次经标记执行：deliver 得 Busy，不打断。
        assert!(manager.pending_handoff("e2e-busy"));
        let second = manager.run_pending_handoff("e2e-busy").await;
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
            manager.pending_handoff("e2e-busy"),
            "失败保留标记，投递路径据此重试"
        );
        manager.retire_core("e2e-busy", true).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 变化点打标后空闲会话立即交接() {
        let dir = tempfile::tempdir().unwrap();
        let manager = crate::core_manager::tests::make_manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(completion_reply("[[SUMMARY]]\n识别即压摘要"))
            .mount(&server)
            .await;
        // 会话在旧指纹下已定档（模拟上一进程的正常收尾）。
        seed_session_with_stale_pin(&dir, &manager, "e2e-mark");
        make_core(&manager, "e2e-mark", &server.uri()).await;
        manager.complete_handoff("e2e-mark", &fingerprint_for(&server.uri()));

        // 变化点打标（目标=新配置指纹），后台立即处理空闲会话。
        let target = fingerprint_for("https://changed.example.com");
        manager.mark_sessions_for_handoff(&target);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !manager.pinned_fingerprint_matches("e2e-mark", &target) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "打标后空闲会话应及时完成交接"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!manager.pending_handoff("e2e-mark"), "完成后清除标记");
        let session = manager.load_session("e2e-mark").unwrap();
        assert_eq!(session.context_summary.as_deref(), Some("识别即压摘要"));
        assert!(requests_of(&server).await >= 1);
        manager.retire_core("e2e-mark", true).await.unwrap();
    }

    async fn requests_of(server: &MockServer) -> usize {
        server.received_requests().await.unwrap().len()
    }
}
