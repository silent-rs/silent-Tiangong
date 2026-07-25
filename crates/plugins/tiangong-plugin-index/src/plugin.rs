//! 索引搜索插件：结构体定义与生命周期钩子实现。
//!
//! [`IndexPlugin`] 通过 [`Plugin::set_workspace`] / [`Plugin::set_trust_mode`] 接收 core
//! 注入的会话上下文；[`IndexManager`] 由构造时注入（app 层单例，跨 Core 共享）或
//! [`IndexPlugin::new`] 自建兜底。
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
/// `workspace` 由 core 在 engine 创建及每次会话目录变更时注入（可变，per-session）；
/// `trust_mode` 由 core 在 register 前通过 `set_trust_mode` 注入（基线共享、单轮隔离）；
/// `index_manager` 由构造时注入——app 层单例（跨 Core 共享同一底层索引缓存与扫描状态），
/// 未注入时 [`IndexPlugin::new`] 自建兜底。
pub struct IndexPlugin {
    /// 当前会话工作目录（可变，由 core 注入）。
    workspace: RwLock<Option<PathBuf>>,
    /// 上次已扫描的工作目录（避免同一目录重复扫描）。
    last_scanned: RwLock<Option<PathBuf>>,
    /// 信任模式解析句柄（FullTrust 时放宽 search_code 路径校验）。
    trust_mode: RwLock<Option<TrustMode>>,
    /// IndexManager 句柄（通常为 app 层单例的 Arc clone；core 不感知其来源）。
    index_manager: RwLock<Option<Arc<IndexManager>>>,
}

impl IndexPlugin {
    /// 构造插件实例：自建 IndexManager，失败时降级为 None 并告警。
    ///
    /// 适用于单测、CLI 兜底等无需跨 Core 共享的场景。生产入口应优先使用
    /// [`IndexPlugin::from_index_manager`] 注入 app 层单例。
    pub fn new() -> Self {
        let im = IndexManager::new()
            .map(Arc::new)
            .map_err(|e| {
                tracing::warn!("IndexManager 初始化失败: {e}");
                e
            })
            .ok();
        Self::from_index_manager(im)
    }

    /// 构造插件实例：注入共享 IndexManager（app 层单例）。
    ///
    /// 多个 IndexPlugin 共享同一 manager 时，底层 per-root 的 `WorkspaceIndex`
    /// 缓存与扫描标志天然共享，消除磁盘锁冲突与缓存重复。`manager` 为 None
    /// 时插件所有写操作与工具执行降级为 no-op（与 [`IndexPlugin::new`] 失败一致）。
    pub fn from_index_manager(manager: Option<Arc<IndexManager>>) -> Self {
        Self {
            workspace: RwLock::new(None),
            last_scanned: RwLock::new(None),
            trust_mode: RwLock::new(None),
            index_manager: RwLock::new(manager),
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

    /// 当前工作区是否正在后台扫描（per-root，经共享 manager 查询）。
    ///
    /// 单例化后跨 IndexPlugin 实例共享：A 对话后台扫描某 root 时，
    /// B 对话查询同一 root 也会得到 true，从而在 `index_search` 中降级。
    pub(crate) fn is_scanning(&self) -> bool {
        let Some(cwd) = self.workspace() else {
            return false;
        };
        self.with_index_manager(|im| im.is_workspace_scanning(&cwd))
            .unwrap_or(false)
    }

    /// 取 IndexManager 引用执行闭包；若未初始化则跳过（用于钩子内的写入操作）。
    fn with_index_manager<R>(&self, f: impl FnOnce(&IndexManager) -> R) -> Option<R> {
        let guard = self.index_manager.read().ok()?;
        let im = guard.as_ref()?;
        Some(f(im))
    }

    /// 对工作区做全量扫描（供 set_workspace / on_session_ready 共用）。
    ///
    /// - 索引不存在：后台全量扫描（不阻塞 IPC 线程）。
    /// - 索引过期（>1 小时）：后台重建，不阻塞当前搜索。
    /// - 索引新鲜：跳过。
    ///
    /// 仅在索引已存在（复用）时同步置位 `last_scanned`；后台扫描完成前由 manager 的
    /// 扫描许可守护，扫描完成后 `workspace_index_exists` 变 true，后续 `set_workspace`
    /// 走复用路径置位 `last_scanned`。
    fn full_scan_workspace(&self, cwd: &str) {
        let root = PathBuf::from(cwd);
        if !root.is_dir() {
            return;
        }
        if !crate::index::workspace_index_exists(&root) {
            // 首次：后台扫描（不阻塞调用线程）。扫描完成后 `workspace_index_exists`
            // 变 true，后续 set_workspace 走复用路径。
            self.spawn_background_scan(root);
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
            self.spawn_background_scan(root);
        }
    }

    /// 后台扫描工作区（首次扫描 / 过期重建共用）。
    ///
    /// 扫描去重经共享 manager 的 [`IndexManager::try_begin_workspace_scan`] 取得许可：
    /// 拿到 permit 才 spawn 线程，线程内持有 permit 直到 `full_scan` 返回（成功或失败）
    /// 后随 permit drop 自动复位——无需手动 `store`。同一 root 已有扫描在进行时返回
    /// `None`，直接跳过。
    fn spawn_background_scan(&self, root: PathBuf) {
        let Some(im) = self.index_manager() else {
            tracing::warn!("Workspace 索引后台扫描跳过：IndexManager 未初始化");
            return;
        };
        let Some(permit) = im.try_begin_workspace_scan(&root) else {
            tracing::debug!(
                workspace = %root.display(),
                "Workspace 索引已有后台扫描在进行，跳过本次"
            );
            return;
        };
        tracing::info!(workspace = %root.display(), "Workspace 索引后台扫描启动");
        std::thread::spawn(move || {
            // permit 持有期间状态保持占用；drop（含 panic 展开）时自动复位。
            let _permit = permit;
            match im.full_scan(&root) {
                Ok(count) => tracing::info!(count, "Workspace 索引后台扫描完成"),
                Err(e) => tracing::warn!("Workspace 索引后台扫描失败: {e}"),
            }
        });
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
        // 后台扫描进行中时也跳过（扫描完成后 workspace_index_exists 变 true，
        // 下次 set_workspace 走复用路径置位 last_scanned）。
        if let Some(ref root) = new_path
            && root.is_dir()
        {
            let already_scanned = self
                .last_scanned
                .read()
                .map(|g| g.as_ref() == Some(root))
                .unwrap_or(false);
            let scanning = self
                .with_index_manager(|im| im.is_workspace_scanning(root))
                .unwrap_or(false);
            if !already_scanned && !scanning {
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
        // 后台扫描进行中时跳过，避免重复 spawn。
        let root = PathBuf::from(&session.cwd);
        if root.is_dir() {
            let scanning = self
                .with_index_manager(|im| im.is_workspace_scanning(&root))
                .unwrap_or(false);
            if scanning {
                return;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexManager;
    use std::time::Instant;

    /// 构造指向临时 base_dir 的共享 manager，避免污染用户索引目录。
    fn shared_manager(temp: &tempfile::TempDir) -> Arc<IndexManager> {
        Arc::new(
            IndexManager::new_with_dir(temp.path().join("index"))
                .expect("IndexManager::new_with_dir"),
        )
    }

    /// 首次扫描工作区（索引不存在）时不应阻塞调用线程：`full_scan_workspace`
    /// 立即返回，扫描在后台线程执行。
    #[test]
    fn full_scan_workspace_does_not_block_on_first_scan() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("lib.rs"), "pub fn demo() {}\n").unwrap();

        let manager = shared_manager(&temp);
        let plugin = IndexPlugin::from_index_manager(Some(manager));
        let started = Instant::now();
        plugin.full_scan_workspace(&workspace.display().to_string());
        let elapsed = started.elapsed();
        // 后台扫描 spawn 后立即返回；即使工作区只有一个文件，调度也应在很短时间内返回。
        assert!(
            elapsed.as_millis() < 200,
            "full_scan_workspace 首次扫描应在后台执行，实际耗时 {elapsed:?}"
        );
    }

    /// 不存在的目录应直接返回且不触发后台扫描（扫描许可不被占用）。
    #[test]
    fn full_scan_workspace_skips_missing_dir() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let manager = shared_manager(&temp);
        let plugin = IndexPlugin::from_index_manager(Some(manager.clone()));
        plugin.set_workspace(Some(std::path::Path::new(
            "/this/path/does/not/exist/abc123",
        )));
        assert!(!plugin.is_scanning(), "不存在的目录不应启动后台扫描");
    }

    /// 重复对同一根目录发起后台扫描，扫描许可应阻止并发 spawn。
    ///
    /// 该测试直接经 manager 持有扫描许可，不依赖 set_workspace 的扫描触发
    /// （后者涉及磁盘索引目录匹配，与测试用的临时 base_dir 不一致）。
    #[test]
    fn spawn_background_scan_is_idempotent_while_scanning() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let manager = shared_manager(&temp);
        // 持有扫描许可模拟扫描进行中：后续 try_begin 应返回 None。
        let _permit = manager
            .try_begin_workspace_scan(&workspace)
            .expect("首次应取得扫描许可");
        assert!(
            manager.try_begin_workspace_scan(&workspace).is_none(),
            "扫描进行中再次取许可应返回 None"
        );
        assert!(manager.is_workspace_scanning(&workspace));
    }

    /// 两个 IndexPlugin 共享同一 manager 时，A 占用的扫描许可对 B 可见（跨 plugin 降级一致）。
    ///
    /// 直接经 manager 占用扫描许可（不依赖 set_workspace 的扫描逻辑与磁盘索引目录），
    /// 断言两个注入相同 manager 的 plugin 查询同一 root 都看到「正在扫描」。
    #[test]
    fn shared_manager_propagates_scanning_across_plugins() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let manager = shared_manager(&temp);
        // 先占用扫描许可（模拟后台扫描进行中），使后续 set_workspace 检测到扫描而跳过，
        // 避免测试自身的后台扫描与本许可竞争。
        let _permit = manager
            .try_begin_workspace_scan(&workspace)
            .expect("取得扫描许可");
        let plugin_a = IndexPlugin::from_index_manager(Some(manager.clone()));
        let plugin_b = IndexPlugin::from_index_manager(Some(manager.clone()));
        plugin_a.set_workspace(Some(&workspace));
        plugin_b.set_workspace(Some(&workspace));

        assert!(plugin_a.is_scanning(), "plugin_a 应看到正在扫描");
        assert!(
            plugin_b.is_scanning(),
            "共享 manager 下 plugin_b 也应看到正在扫描"
        );
    }
}
