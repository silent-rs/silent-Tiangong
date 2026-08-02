//! Index 存储协调器（仅 sidecar 内部）。
//!
//! 协调 per-workspace / per-session tantivy 索引：
//! - `workspaces`：工作区文件全文索引（跨会话共享，per-root 缓存 + 扫描去重）
//! - `sessions`：对话历史索引（per-session）
//!
//! 复用 `tiangong_plugin_index_protocol` 的类型（IndexScope/IndexHit/TurnData 等），
//! 避免内部类型与协议类型重复。全部 native 资源（tantivy mmap）在 sidecar 进程内持有，
//! 经 IPC 暴露给 WASM。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use dashmap::DashMap;
use tiangong_plugin_index_protocol::{IndexHit, IndexScope, SessionIndexHit, TurnData};

use self::session_index::SessionIndex;
use self::workspace_index::WorkspaceIndex;

pub(crate) mod session_index;
pub(crate) mod tantivy_schema;
pub(crate) mod workspace_index;

#[derive(Debug, Clone)]
pub struct IndexQuery {
    pub text: String,
    pub scope: IndexScope,
    pub limit: usize,
}

impl IndexQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            scope: IndexScope::All,
            limit: 20,
        }
    }

    pub fn with_scope(mut self, scope: IndexScope) -> Self {
        self.scope = scope;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexMeta {
    pub root: String,
    pub entry_count: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceIndexInfo {
    pub id: String,
    pub root: String,
    pub entry_count: usize,
    pub updated_at: String,
}

pub struct IndexManager {
    workspaces: DashMap<String, Arc<std::sync::Mutex<WorkspaceIndex>>>,
    sessions: DashMap<String, Arc<std::sync::Mutex<SessionIndex>>>,
    /// 每个 workspace root 的后台扫描标志（key = `workspace_key(root)`）。
    scanning_roots: std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>,
    base_dir: PathBuf,
}

/// 工作区扫描资格许可（RAII）。
///
/// 由 [`IndexManager::try_begin_workspace_scan`] 原子取得，代表「已获得对该工作区
/// 执行后台扫描的独占资格」。持有期间 [`IndexManager::is_workspace_scanning`] 对该
/// root 返回 true；drop 时自动复位，无需调用方手动释放——覆盖正常返回、错误返回、
/// 以及线程 panic 展开三种情况。
pub struct WorkspaceScanPermit {
    scanning: Arc<AtomicBool>,
}

impl Drop for WorkspaceScanPermit {
    fn drop(&mut self) {
        self.scanning.store(false, Ordering::Release);
    }
}

impl IndexManager {
    pub fn new() -> Result<Self> {
        let base_dir = Self::default_dir();
        Self::with_base_dir(base_dir)
    }

    pub fn new_with_dir(base_dir: PathBuf) -> Result<Self> {
        Self::with_base_dir(base_dir)
    }

    fn with_base_dir(base_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&base_dir).context("创建索引基础目录失败")?;
        Ok(Self {
            workspaces: DashMap::new(),
            sessions: DashMap::new(),
            scanning_roots: std::sync::Mutex::new(std::collections::HashMap::new()),
            base_dir,
        })
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    fn scanning_flag_for(&self, root: &Path) -> Arc<AtomicBool> {
        let key = workspace_key(root);
        let mut map = self.scanning_roots.lock().expect("scanning_roots 锁中毒");
        Arc::clone(
            map.entry(key)
                .or_insert_with(|| Arc::new(AtomicBool::new(false))),
        )
    }

    pub fn try_begin_workspace_scan(&self, root: &Path) -> Option<WorkspaceScanPermit> {
        let scanning = self.scanning_flag_for(root);
        scanning
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| WorkspaceScanPermit { scanning })
    }

    pub fn is_workspace_scanning(&self, root: &Path) -> bool {
        let map = self.scanning_roots.lock().expect("scanning_roots 锁中毒");
        map.get(&workspace_key(root))
            .is_some_and(|f| f.load(Ordering::Acquire))
    }

    pub fn get_or_create_workspace_index(
        &self,
        root: &Path,
    ) -> Result<Arc<std::sync::Mutex<WorkspaceIndex>>> {
        let key = workspace_key(root);
        if let Some(entry) = self.workspaces.get(&key) {
            return Ok(Arc::clone(entry.value()));
        }

        let index = WorkspaceIndex::open_or_create(root, &self.base_dir)?;
        let index = Arc::new(std::sync::Mutex::new(index));
        self.workspaces.insert(key, Arc::clone(&index));
        Ok(index)
    }

    pub fn get_or_create_session_index(
        &self,
        session_id: &str,
    ) -> Result<Arc<std::sync::Mutex<SessionIndex>>> {
        if let Some(entry) = self.sessions.get(session_id) {
            return Ok(Arc::clone(entry.value()));
        }

        let index = SessionIndex::open_or_create(session_id, &self.base_dir)?;
        let index = Arc::new(std::sync::Mutex::new(index));
        self.sessions
            .insert(session_id.to_string(), Arc::clone(&index));
        Ok(index)
    }

    pub fn full_scan(&self, root: &Path) -> Result<usize> {
        let index = self.get_or_create_workspace_index(root)?;
        let mut guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Workspace 索引锁获取失败: {}", e))?;
        guard.full_scan()
    }

    pub fn search(&self, root: &Path, query: &IndexQuery) -> Result<Vec<IndexHit>> {
        let index = self.get_or_create_workspace_index(root)?;
        let guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Workspace 索引锁获取失败: {}", e))?;
        let hits = guard.search(&query.text, query.limit)?;
        Ok(hits
            .into_iter()
            .map(|h| IndexHit {
                path: h.path,
                language: h.language,
                scope: IndexScope::Workspace,
            })
            .collect())
    }

    pub fn search_session(
        &self,
        session_id: &str,
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<SessionIndexHit>> {
        let index = self.get_or_create_session_index(session_id)?;
        let guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Session 索引锁获取失败: {}", e))?;
        let hits = guard.search(query_text, limit)?;
        Ok(hits
            .into_iter()
            .map(|h| SessionIndexHit {
                turn_id: h.turn_id,
                role: h.role,
                content: h.content,
            })
            .collect())
    }

    /// 批量写入 turn（不自动 commit），需在调用后手动 commit
    pub fn index_turn_batch(&self, session_id: &str, turns: &[TurnData]) -> Result<()> {
        if turns.is_empty() {
            return Ok(());
        }
        let index = self.get_or_create_session_index(session_id)?;
        let mut guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Session 索引锁获取失败: {}", e))?;
        for turn in turns {
            guard.index_turn(turn)?;
        }
        guard.commit()
    }

    // 以下方法为完整 API，当前主要供测试与未来增量更新使用。
    #[allow(dead_code)]
    pub fn index_turn(&self, session_id: &str, turn: &TurnData) -> Result<()> {
        let index = self.get_or_create_session_index(session_id)?;
        let mut guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Session 索引锁获取失败: {}", e))?;
        guard.index_turn(turn)?;
        guard.commit()
    }

    #[allow(dead_code)]
    pub fn session_turn_count(&self, session_id: &str) -> Result<usize> {
        let index = self.get_or_create_session_index(session_id)?;
        let guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Session 索引锁获取失败: {}", e))?;
        Ok(guard.turn_count())
    }

    #[allow(dead_code)]
    pub fn update_file(&self, root: &Path, path: &Path) -> Result<()> {
        let index = self.get_or_create_workspace_index(root)?;
        let mut guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Workspace 索引锁获取失败: {}", e))?;
        guard.index_file(path)
    }

    #[allow(dead_code)]
    pub fn remove_file(&self, root: &Path, rel_path: &str) -> Result<()> {
        let index = self.get_or_create_workspace_index(root)?;
        let mut guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Workspace 索引锁获取失败: {}", e))?;
        guard.remove_file(rel_path)
    }

    #[allow(dead_code)]
    pub fn workspace_entry_count(&self, root: &Path) -> Result<usize> {
        let index = self.get_or_create_workspace_index(root)?;
        let guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Workspace 索引锁获取失败: {}", e))?;
        Ok(guard.entry_count())
    }

    pub fn finalize_session_index(&self, session_id: &str) -> Result<()> {
        if let Some(entry) = self.sessions.get(session_id) {
            let mut guard = entry
                .lock()
                .map_err(|e| anyhow::anyhow!("Session 索引锁获取失败: {}", e))?;
            guard.finalize()?;
        }
        Ok(())
    }

    pub fn list_workspace_indexes(&self) -> Result<Vec<WorkspaceIndexInfo>> {
        let ws_dir = self.base_dir.join("workspaces");
        if !ws_dir.exists() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for entry in fs::read_dir(&ws_dir)? {
            let entry = entry?;
            let tantivy_path = entry.path().join("tantivy");
            if !tantivy_path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let meta_path = entry.path().join("meta.json");
            let (root, entry_count, updated_at) = if meta_path.exists() {
                let content = fs::read_to_string(&meta_path).unwrap_or_default();
                let meta: IndexMeta = serde_json::from_str(&content).unwrap_or(IndexMeta {
                    root: String::new(),
                    entry_count: 0,
                    updated_at: String::new(),
                });
                (meta.root, meta.entry_count, meta.updated_at)
            } else {
                (String::new(), 0, String::new())
            };
            result.push(WorkspaceIndexInfo {
                id: name,
                root,
                entry_count,
                updated_at,
            });
        }
        result.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(result)
    }

    /// 删除指定工作区的索引。
    ///
    /// `root` 用于清理共享 manager 的内存缓存（缓存以 `workspace_key(root)` 为键）
    /// 并由调用方据此取得扫描许可；`workspace_id`（`hash_path(root)`）用于定位磁盘
    /// 索引目录。二者来自不同的键派生，必须分别传入。
    pub fn delete_workspace_index(&self, root: &Path, workspace_id: &str) -> Result<()> {
        let expected_id = workspace_index::hash_path(root);
        if workspace_id != expected_id {
            return Err(anyhow::anyhow!(
                "工作区路径与索引 ID 不匹配：root={} workspace_id={} expected={}",
                root.display(),
                workspace_id,
                expected_id
            ));
        }
        self.workspaces.remove(&workspace_key(root));
        let dir = self.base_dir.join("workspaces").join(workspace_id);
        if dir.exists() {
            fs::remove_dir_all(&dir).context("删除 Workspace 索引失败")?;
        }
        Ok(())
    }

    /// 检查 workspace 索引是否已存在
    pub fn workspace_index_exists(&self, root: &Path) -> bool {
        let tantivy_dir = self.workspace_index_dir(root);
        tantivy_dir.is_dir()
    }

    fn workspace_index_dir(&self, root: &Path) -> PathBuf {
        let workspace_id = workspace_index::hash_path(root);
        self.base_dir
            .join("workspaces")
            .join(workspace_id)
            .join("tantivy")
    }

    fn default_dir() -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".tiangong").join("index")
    }
}

/// 工作区在内存映射（`workspaces` / `scanning_roots`）中的统一 key。
///
/// 规范化路径以消除 `.`/`..`/软链接/相对路径导致的等价目录生成不同 key 问题，
/// 避免绕过扫描去重。canonicalize 失败（如路径暂不存在）时退回原始路径。
fn workspace_key(root: &Path) -> String {
    root.to_path_buf()
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// 并发为同一路径申请扫描许可，必须只有一个线程成功取得（去重原子性）。
    #[test]
    fn try_begin_workspace_scan_only_one_wins_concurrently() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let manager =
            Arc::new(IndexManager::new_with_dir(temp.path().join("index")).expect("manager"));
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();

        const N: usize = 8;
        let barrier = Arc::new(std::sync::Barrier::new(N));
        let manager_clones: Vec<_> = (0..N).map(|_| Arc::clone(&manager)).collect();
        let mut handles = Vec::new();
        for mgr in manager_clones {
            let barrier = Arc::clone(&barrier);
            let root = root.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                mgr.try_begin_workspace_scan(&root)
            }));
        }
        let permits: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let wins = permits.iter().filter(|p| p.is_some()).count();
        assert_eq!(wins, 1, "并发申请扫描许可时只应有一个成功，实际 {wins}");
        assert!(manager.is_workspace_scanning(&root));

        let other = temp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert!(!manager.is_workspace_scanning(&other));
    }

    /// 许可释放（drop）后状态复位，可再次取得扫描许可。
    #[test]
    fn permit_release_allows_reacquire() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let manager =
            Arc::new(IndexManager::new_with_dir(temp.path().join("index")).expect("manager"));
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();

        let permit = manager
            .try_begin_workspace_scan(&root)
            .expect("首次应取得扫描许可");
        assert!(manager.is_workspace_scanning(&root));
        assert!(
            manager.try_begin_workspace_scan(&root).is_none(),
            "持有许可期间再次取应返回 None"
        );
        drop(permit);
        assert!(
            !manager.is_workspace_scanning(&root),
            "许可 drop 后状态应复位"
        );
        manager
            .try_begin_workspace_scan(&root)
            .expect("释放后应可再次取得许可");
    }

    /// 模拟扫描返回错误（panic），许可仍应自动复位，不阻塞后续扫描。
    #[test]
    fn permit_auto_releases_on_panic() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let manager =
            Arc::new(IndexManager::new_with_dir(temp.path().join("index")).expect("manager"));
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();

        let permit = manager
            .try_begin_workspace_scan(&root)
            .expect("取得扫描许可");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _permit = permit;
            panic!("模拟扫描失败");
        }));
        assert!(result.is_err(), "应捕获到 panic");
        assert!(
            !manager.is_workspace_scanning(&root),
            "panic 展开后许可应自动复位"
        );
    }

    /// 等价路径（`.`、相对形式）经规范化后应视为同一工作区，扫描去重不被绕过。
    #[test]
    fn equivalent_paths_share_scan_state() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let manager =
            Arc::new(IndexManager::new_with_dir(temp.path().join("index")).expect("manager"));
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();

        let _permit = manager
            .try_begin_workspace_scan(&root)
            .expect("取得扫描许可");

        let dot = root.join(".");
        assert!(
            manager.try_begin_workspace_scan(&dot).is_none(),
            "等价路径（.）应视为同一工作区，去重不被绕过"
        );
    }

    /// 删除工作区索引后，共享 manager 的内存缓存被清理、磁盘目录被删除。
    #[test]
    fn delete_workspace_index_clears_cache_and_disk_index() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let base_dir = temp.path().join("index");
        let manager = Arc::new(IndexManager::new_with_dir(base_dir.clone()).expect("manager"));

        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).expect("create workspace");
        std::fs::write(root.join("lib.rs"), "pub fn demo() {}\n").expect("write source file");

        let count = manager.full_scan(&root).expect("full_scan");
        assert_eq!(count, 1);
        let old_index = manager
            .get_or_create_workspace_index(&root)
            .expect("get old index");

        let workspace_id = workspace_index::hash_path(&root);
        let workspace_dir = base_dir.join("workspaces").join(&workspace_id);
        assert!(workspace_dir.is_dir(), "删除前工作区索引目录应存在");

        manager
            .delete_workspace_index(&root, &workspace_id)
            .expect("delete index");

        assert!(!workspace_dir.exists(), "删除后工作区索引目录不应存在");

        let new_index = manager
            .get_or_create_workspace_index(&root)
            .expect("recreate index");
        assert!(
            !Arc::ptr_eq(&old_index, &new_index),
            "删除后不应继续返回旧的缓存对象"
        );
    }

    /// root 与 workspace_id 不一致时拒绝删除（防御性校验）。
    #[test]
    fn delete_workspace_index_rejects_mismatched_id() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let base_dir = temp.path().join("index");
        let manager = Arc::new(IndexManager::new_with_dir(base_dir.clone()).expect("manager"));

        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).expect("create workspace");
        std::fs::write(root.join("lib.rs"), "pub fn demo() {}\n").expect("write source file");
        manager.full_scan(&root).expect("full_scan");

        let old_index = manager
            .get_or_create_workspace_index(&root)
            .expect("get old index");
        let correct_id = workspace_index::hash_path(&root);
        let workspace_dir = base_dir.join("workspaces").join(&correct_id);
        assert!(workspace_dir.is_dir(), "删除前工作区索引目录应存在");

        let err = manager
            .delete_workspace_index(&root, "deadbeefdeadbeef")
            .expect_err("mismatched id should fail");
        assert!(
            err.to_string().contains("不匹配"),
            "应拒绝不匹配的 workspace_id，实际错误: {err}"
        );

        assert!(workspace_dir.is_dir(), "错误请求不应删除正确索引目录");
        let current_index = manager
            .get_or_create_workspace_index(&root)
            .expect("get cached index");
        assert!(
            Arc::ptr_eq(&old_index, &current_index),
            "错误请求不应清理正确的缓存对象"
        );
    }
}
