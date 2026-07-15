//! 索引搜索插件：结构体定义与生命周期钩子实现。
//!
//! [`IndexPlugin`] 通过 [`Plugin::set_workspace`] / [`Plugin::set_trust_mode`] 接收 core
//! 注入的会话上下文；[`IndexManager`] 在 [`IndexPlugin::new`] 时自建并私有持有。
//!
//! 生命周期钩子接管原 core 对 `IndexManager` 的全部写入与维护：
//! - [`Plugin::set_workspace`]：工作区变更后重扫索引（原 `on_cwd_changed` 的职责）。
//! - [`Plugin::on_session_ready`]：首次全量扫描工作区索引（原 core/mod.rs 初始扫描）。
//! - [`Plugin::on_turn_finished`]：批量写入本轮对话索引（原 `index_turn_messages`）。
//! - [`Plugin::on_session_ended`]：finalize Session 索引。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::index::{IndexManager, TurnData};
use tiangong_core::core::Plugin;
use tiangong_core::permission::TrustMode;
use tiangong_core::session::{MessageRole, Session};
use tiangong_core::tool_override::PromptSectionProvider;

/// 索引搜索插件。
///
/// `workspace` 由 core 在 engine 创建及每次会话目录变更时注入（可变）；
/// `trust_mode` 由 core 在 register 前通过 `set_trust_mode` 注入（基线共享、单轮隔离）；
/// `index_manager` 在构造时自建（失败降级为 None，工具执行与钩子内兜底）。
pub struct IndexPlugin {
    /// 当前会话工作目录（可变，由 core 注入）。
    workspace: RwLock<Option<PathBuf>>,
    /// 上次已扫描的工作目录（避免同一目录重复扫描）。
    last_scanned: RwLock<Option<PathBuf>>,
    /// 信任模式解析句柄（FullTrust 时放宽 search_code 路径校验）。
    trust_mode: RwLock<Option<TrustMode>>,
    /// 自建并私有持有的 IndexManager（core 不感知）。
    index_manager: RwLock<Option<Arc<IndexManager>>>,
}

impl IndexPlugin {
    /// 构造插件实例：自建 IndexManager，失败时降级为 None 并告警。
    pub fn new() -> Self {
        let im = IndexManager::new()
            .map(Arc::new)
            .map_err(|e| {
                tracing::warn!("IndexManager 初始化失败: {e}");
                e
            })
            .ok();
        Self {
            workspace: RwLock::new(None),
            last_scanned: RwLock::new(None),
            trust_mode: RwLock::new(None),
            index_manager: RwLock::new(im),
        }
    }

    /// 读取当前工作目录的快照。
    pub(crate) fn workspace(&self) -> Option<PathBuf> {
        self.workspace.read().ok()?.clone()
    }

    /// 读取 IndexManager 的 Arc 克隆（供 handler 查询用）。
    pub(crate) fn index_manager(&self) -> Option<Arc<IndexManager>> {
        self.index_manager.read().ok()?.clone()
    }

    /// 当前是否处于完全信任模式。
    pub(crate) fn is_full_trust(&self) -> bool {
        let Ok(handle) = self.trust_mode.read() else {
            return false;
        };
        let Some(tm) = handle.as_ref() else {
            return false;
        };
        *tm == TrustMode::FullTrust
    }

    /// 取 IndexManager 引用执行闭包；若未初始化则跳过（用于钩子内的写入操作）。
    fn with_index_manager<R>(&self, f: impl FnOnce(&IndexManager) -> R) -> Option<R> {
        let guard = self.index_manager.read().ok()?;
        let im = guard.as_ref()?;
        Some(f(im))
    }

    /// 对工作区做全量扫描（供 set_workspace / on_session_ready 共用）。
    ///
    /// - 索引不存在：同步全量扫描（首次使用，阻塞直到完成）。
    /// - 索引过期（>1 小时）：后台重建，不阻塞当前搜索。
    /// - 索引新鲜：跳过。
    ///
    /// 仅在索引已存在（复用或后台重建）或同步首次扫描成功时置位 `last_scanned`，
    /// 使后续 `set_workspace` 对同一目录不再重复扫描；扫描失败时不置位，允许下次重试。
    fn full_scan_workspace(&self, cwd: &str) {
        let root = PathBuf::from(cwd);
        if !root.is_dir() {
            return;
        }
        if !crate::index::workspace_index_exists(&root) {
            // 首次：同步全量扫描。仅在成功后置位 last_scanned，失败则允许下次重试。
            let scanned = self.with_index_manager(|im| match im.full_scan(&root) {
                Ok(count) => {
                    tracing::info!(count, "Workspace 初始索引扫描完成");
                    true
                }
                Err(e) => {
                    tracing::warn!("Workspace 初始索引扫描失败: {e}");
                    false
                }
            });
            if scanned.unwrap_or(false)
                && let Ok(mut guard) = self.last_scanned.write()
            {
                guard.clone_from(&Some(root));
            }
            return;
        }
        // 索引已存在——复用，置位 last_scanned。
        if let Ok(mut guard) = self.last_scanned.write() {
            guard.clone_from(&Some(root.clone()));
        }
        // 检查索引年龄，过期则后台重建（不阻塞搜索）
        const STALE_THRESHOLD_SECS: u64 = 3600; // 1 小时
        if let Some(age) = crate::index::workspace_index_age_secs(&root)
            && age > STALE_THRESHOLD_SECS
        {
            tracing::info!(age_secs = age, "Workspace 索引过期，后台重建");
            let im = self.index_manager();
            let root_clone = root.clone();
            std::thread::spawn(move || {
                if let Some(im) = im
                    && let Err(e) = im.full_scan(&root_clone)
                {
                    tracing::warn!("Workspace 索引后台重建失败: {e}");
                }
            });
        }
    }
}

impl Default for IndexPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for IndexPlugin {
    fn id(&self) -> &str {
        "index"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        let new_path = workspace.map(|p| p.to_path_buf());
        if let Ok(mut guard) = self.workspace.write() {
            *guard = new_path.clone();
        }
        // 工作区变更后重扫索引（原 on_cwd_changed 的职责）。
        // 仅当新路径与上次已扫描路径不同时才触发，避免重复扫描。
        // 复用 full_scan_workspace 的检查策略：索引已存在则复用，不存在才同步全量扫描。
        if let Some(ref root) = new_path
            && root.is_dir()
        {
            let already_scanned = self
                .last_scanned
                .read()
                .map(|g| g.as_ref() == Some(root))
                .unwrap_or(false);
            if !already_scanned {
                self.full_scan_workspace(&root.display().to_string());
            }
        }
    }

    fn set_trust_mode(&self, trust: TrustMode) {
        if let Ok(mut guard) = self.trust_mode.write() {
            *guard = Some(trust);
        }
    }

    // register 留空：IndexManager 在 new() 时自建，工具规格 / 工具覆盖 / Prompt 段落
    // 由 core 通过 supertrait 自动收集。

    fn on_session_ready(&self, session: &mut Session) {
        // set_workspace 已在 engine 初始化时对当前 cwd 触发过扫描（last_scanned 已置位）。
        // 若 set_workspace 因扫描失败未置位，此处兜底重试一次。
        let root = PathBuf::from(&session.cwd);
        if root.is_dir() {
            let already_scanned = self
                .last_scanned
                .read()
                .map(|g| g.as_ref() == Some(&root))
                .unwrap_or(false);
            if !already_scanned {
                self.full_scan_workspace(&session.cwd);
            }
        }
    }

    fn on_turn_finished(&self, session: &mut Session, turn_start_idx: usize) {
        let turns: Vec<TurnData> = session.messages[turn_start_idx..]
            .iter()
            .filter_map(|msg| {
                let role = match msg.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                    MessageRole::System => return None,
                };
                Some(TurnData {
                    turn_id: msg.id.clone(),
                    workspace_id: session.cwd.clone(),
                    role: role.to_string(),
                    content: msg.text_content(),
                    topics: Vec::new(),
                    entity_names: Vec::new(),
                })
            })
            .collect();
        if turns.is_empty() {
            return;
        }
        self.with_index_manager(|im| {
            if let Err(e) = im.index_turn_batch(&session.id, &turns) {
                tracing::warn!("Session 索引批量写入失败: {e}");
            }
        });
    }

    fn on_session_ended(&self, session: &mut Session) {
        self.with_index_manager(|im| {
            if let Err(e) = im.finalize_session_index(&session.id) {
                tracing::warn!("Session 索引 finalize 失败: {e}");
            }
        });
    }
}

// 注入检索工具使用指引：以操作策略为主，说明 search_code 与 index_search 的配合用法。
impl PromptSectionProvider for IndexPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        vec![
            "## 检索工具使用指引\n\
             - search_code 用于精确文本/正则检索，优先使用 rg；若环境缺失 rg，工具会自动\
             回退到 grep，可能较慢。调用 search_code 时应尽量指定更小的 path 和更精确的\
             pattern，避免全仓搜索导致超时。\n\
             - index_search 用于基于索引的语义检索（工作区文件 + 对话历史），速度更快但\
             受索引覆盖范围限制；需要精确定位某行代码时优先用 index_search 缩小范围，\
             再用 search_code 取精确行号。"
                .to_string(),
        ]
    }
}
