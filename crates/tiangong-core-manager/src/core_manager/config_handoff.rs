//! 配置交接的**状态记账**：待交接标记、首检记录与定档指纹的存取。
//!
//! 编排（变化点打标后的即时处理、投递消息前的交接执行、压缩发起与收敛
//! 判据）在宿主 app 层（`src-tauri/src/config_handoff.rs`）——manager 恪守
//! 记账定位（与 issue #245 一致），不执行压缩；core 只保留通用压缩能力
//! （手动压缩命令），不感知「交接」概念。
//!
//! 运行期是**标记制**：变化发生的地方（插件装卸/升级/启停、模型切换）
//! 给活跃会话打标并立即尝试处理；投递消息路径只查内存标记。兜底是
//! **首检指纹比对**：每会话每进程首次投递前算一次执行配置指纹与落盘
//! 定档比对，防两类漏报——进程重启丢内存标记、未走正常变化入口的变化。
//! 定档指纹持久化在 `storage_root/config-fingerprints.json`，不进会话
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

use tiangong_llm::ModelEndpoint;

use crate::CoreManager;

/// 定档指纹存储文件（storage_root 下，session_id → 指纹摘要）。
const FINGERPRINTS_FILE: &str = "config-fingerprints.json";

/// 一次交接检查的结果（编排层产出，这里仅承载类型供宿主与调用方使用）。
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
    /// 待交接标记：session_id → 目标指纹（变化点写入，交接完成清除、
    /// 失败/忙保留供投递路径重试）。
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

impl CoreManager {
    /// 写入待交接标记（变化点与交接失败时调用；幂等，后写覆盖目标指纹）。
    pub fn set_pending_handoff(&self, session_ids: &[String], target_fingerprint: &str) {
        if session_ids.is_empty() {
            return;
        }
        self.lock_handoff_state().pending.extend(
            session_ids
                .iter()
                .map(|id| (id.clone(), target_fingerprint.to_string())),
        );
    }

    /// 读取待交接标记的目标指纹（投递路径快查；无标记返回 None）。
    pub fn pending_handoff(&self, session_id: &str) -> Option<String> {
        self.lock_handoff_state().pending.get(session_id).cloned()
    }

    /// 本会话是否尚未做过首检指纹兜底比对（每会话每进程一次）。
    pub fn handoff_bootstrap_needed(&self, session_id: &str) -> bool {
        !self.lock_handoff_state().checked.contains(session_id)
    }

    /// 标记本会话已完成首检（首检编排由宿主执行，这里只记账）。
    pub fn mark_handoff_checked(&self, session_id: &str) {
        self.lock_handoff_state()
            .checked
            .insert(session_id.to_string());
    }

    /// 交接完成：定档目标指纹（落盘）并清除待交接标记。
    pub fn complete_handoff(&self, session_id: &str, fingerprint: &str) {
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

    /// 定档指纹是否与给定值一致（首检兜底比对的读侧）。
    pub fn pinned_fingerprint_matches(&self, session_id: &str, fingerprint: &str) -> bool {
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
    fn 标记与定档的状态记账roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let manager = crate::core_manager::tests::make_manager(&dir);

        // 待交接标记：写入、读取、完成清除。
        assert_eq!(manager.pending_handoff("s1"), None);
        manager.set_pending_handoff(&["s1".to_string()], "fp-target");
        assert_eq!(manager.pending_handoff("s1").as_deref(), Some("fp-target"));
        manager.set_pending_handoff(&["s1".to_string()], "fp-newer");
        assert_eq!(
            manager.pending_handoff("s1").as_deref(),
            Some("fp-newer"),
            "变化点再次打标覆盖目标"
        );
        manager.complete_handoff("s1", "fp-newer");
        assert_eq!(manager.pending_handoff("s1"), None, "完成清除标记");
        assert!(manager.pinned_fingerprint_matches("s1", "fp-newer"));

        // 首检记录：一次性。
        assert!(manager.handoff_bootstrap_needed("s2"));
        manager.mark_handoff_checked("s2");
        assert!(!manager.handoff_bootstrap_needed("s2"));

        // 定档持久化：新实例懒加载恢复；首检记录仅进程内，重启后重做首检。
        let restored = crate::core_manager::tests::make_manager(&dir);
        assert!(restored.pinned_fingerprint_matches("s1", "fp-newer"));
        assert!(
            restored.handoff_bootstrap_needed("s2"),
            "首检记录不落盘，新进程重做首检兜底"
        );
        restored.forget_config_fingerprint("s1");
        assert!(!restored.pinned_fingerprint_matches("s1", "fp-newer"));
        let again = crate::core_manager::tests::make_manager(&dir);
        assert!(
            !again.pinned_fingerprint_matches("s1", "fp-newer"),
            "遗忘须落盘"
        );
    }
}
