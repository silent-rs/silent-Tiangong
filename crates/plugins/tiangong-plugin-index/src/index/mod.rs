use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use dashmap::DashMap;

use self::session_index::SessionIndex;
use self::workspace_index::WorkspaceIndex;

mod session_index;
mod tantivy_schema;
mod watcher;
mod workspace_index;

#[derive(Debug, Clone, PartialEq)]
pub enum IndexScope {
    Workspace,
    Session,
    All,
}

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

#[derive(Debug, Clone)]
pub struct IndexHit {
    pub path: String,
    pub language: String,
    pub source: IndexScope,
}

#[derive(Debug, Clone)]
pub struct TurnData {
    pub turn_id: String,
    pub workspace_id: String,
    pub role: String,
    pub content: String,
    pub topics: Vec<String>,
    pub entity_names: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SessionIndexHit {
    pub turn_id: String,
    pub role: String,
    pub content: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
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
    /// 单例化后由所有 IndexPlugin 共享，使 A 对话扫描时 B 对话的 index_search 也能降级。
    scanning_roots: std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>,
    base_dir: PathBuf,
}

/// 工作区扫描资格许可（RAII）。
///
/// 由 [`IndexManager::try_begin_workspace_scan`] 原子取得，代表「已获得对该工作区
/// 执行后台扫描的独占资格」。持有期间 [`IndexManager::is_workspace_scanning`] 对该
/// root 返回 true；drop 时自动复位，无需调用方手动释放——覆盖正常返回、错误返回、
/// 以及线程 panic 展开三种情况。
///
/// 设计目的：把扫描状态的获取/释放收敛到 [`IndexManager`]，调用方只持 permit，
/// 无法接触底层原子标志，杜绝漏复位或绕过去重。
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

    /// 取（或创建）指定 root 的后台扫描标志。同一 manager + 同一 root 返回同一 Arc。
    ///
    /// 取（或创建）指定 root 的后台扫描标志。同一 manager + 同一 root 返回同一 Arc。
    ///
    /// 私有：扫描状态的获取与释放应由 [`IndexManager::try_begin_workspace_scan`]
    /// 返回的许可统一管理，避免调用方各自 `swap`/`store` 造成漏复位或绕过去重。
    ///
    /// 经 `Mutex<HashMap>` 保护 get-or-create：并发首次访问时锁保证只有一个线程
    /// 创建该 root 的标志，其余线程在锁内读到已存在的 Arc。此前用 `DashMap::entry()`
    /// 时并发 entry() 会竞争创建不同 Arc，CAS 各自成功、去重失效。
    fn scanning_flag_for(&self, root: &Path) -> Arc<AtomicBool> {
        let key = workspace_key(root);
        let mut map = self.scanning_roots.lock().expect("scanning_roots 锁中毒");
        Arc::clone(
            map.entry(key)
                .or_insert_with(|| Arc::new(AtomicBool::new(false))),
        )
    }

    /// 尝试取得指定工作区的扫描资格。
    ///
    /// 返回 `Some(permit)` 表示获得资格（原子地由空闲置为占用），调用方在后台线程
    /// 持有该 permit 执行 `full_scan`；permit 被 drop 时（正常返回、错误返回、甚至
    /// panic 展开时）自动复位状态。返回 `None` 表示该工作区已有扫描在进行。
    ///
    /// 扫描去重的唯一入口：调用方无法接触底层原子标志。标志由 [`scanning_flag_for`]
    /// 在锁下唯一创建，故 CAS 一定作用在同一 Arc 上，保证全局只有一个调用方能占用。
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
                source: IndexScope::Workspace,
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

    pub fn index_turn(&self, session_id: &str, turn: &TurnData) -> Result<()> {
        let index = self.get_or_create_session_index(session_id)?;
        let mut guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Session 索引锁获取失败: {}", e))?;
        guard.index_turn(turn)?;
        guard.commit()
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

    pub fn finalize_session_index(&self, session_id: &str) -> Result<()> {
        if let Some(entry) = self.sessions.get(session_id) {
            let mut guard = entry
                .lock()
                .map_err(|e| anyhow::anyhow!("Session 索引锁获取失败: {}", e))?;
            guard.finalize()?;
        }
        Ok(())
    }

    pub fn update_file(&self, root: &Path, path: &Path) -> Result<()> {
        let index = self.get_or_create_workspace_index(root)?;
        let mut guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Workspace 索引锁获取失败: {}", e))?;
        guard.index_file(path)
    }

    pub fn remove_file(&self, root: &Path, rel_path: &str) -> Result<()> {
        let index = self.get_or_create_workspace_index(root)?;
        let mut guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Workspace 索引锁获取失败: {}", e))?;
        guard.remove_file(rel_path)
    }

    pub fn workspace_entry_count(&self, root: &Path) -> Result<usize> {
        let index = self.get_or_create_workspace_index(root)?;
        let guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Workspace 索引锁获取失败: {}", e))?;
        Ok(guard.entry_count())
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
                // 没有 meta.json 的旧索引，标记为未知来源
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

    pub fn delete_workspace_index(&self, workspace_id: &str) -> Result<()> {
        self.workspaces.remove(workspace_id);
        let dir = self.base_dir.join("workspaces").join(workspace_id);
        if dir.exists() {
            fs::remove_dir_all(&dir).context("删除 Workspace 索引失败")?;
        }
        Ok(())
    }

    pub fn rebuild_workspace_index(&self, root: &Path) -> Result<usize> {
        self.full_scan(root)
    }

    pub fn session_turn_count(&self, session_id: &str) -> Result<usize> {
        let index = self.get_or_create_session_index(session_id)?;
        let guard = index
            .lock()
            .map_err(|e| anyhow::anyhow!("Session 索引锁获取失败: {}", e))?;
        Ok(guard.turn_count())
    }

    pub fn delete_session_index(&self, session_id: &str) -> Result<()> {
        self.sessions.remove(session_id);
        let dir = self.base_dir.join("sessions").join(session_id);
        if dir.exists() {
            fs::remove_dir_all(&dir).context("删除 Session 索引失败")?;
        }
        Ok(())
    }

    fn default_dir() -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".tiangong").join("index")
    }
}

// index_search 工具的规格与执行逻辑已下沉到 tiangong-plugin-index 插件，
// core 仅保留 IndexManager 及其底层 API（search / search_session / full_scan /
// index_turn_batch / finalize_session_index 等）供插件与 GUI API 复用。

// ── GUI API ──

pub fn list_workspace_indexes_for_gui() -> Result<Vec<WorkspaceIndexInfo>> {
    let manager = IndexManager::new()?;
    manager.list_workspace_indexes()
}

pub fn delete_workspace_index_for_gui(workspace_id: &str) -> Result<()> {
    let manager = IndexManager::new()?;
    manager.delete_workspace_index(workspace_id)
}

/// 检查 workspace 索引是否已存在
pub fn workspace_index_exists(root: &Path) -> bool {
    let tantivy_dir = workspace_index_dir(root);
    tantivy_dir.is_dir()
}

/// 索引年龄（秒）。返回 None 表示索引不存在。
/// 用于判断索引是否过期（文件已修改但索引未更新）。
pub fn workspace_index_age_secs(root: &Path) -> Option<u64> {
    let tantivy_dir = workspace_index_dir(root);
    let meta = tantivy_dir.metadata().ok()?;
    let modified = meta.modified().ok()?;
    let elapsed = std::time::SystemTime::now().duration_since(modified).ok()?;
    Some(elapsed.as_secs())
}

/// 检查 session 索引是否已存在
pub fn session_index_exists(session_id: &str) -> bool {
    let base_dir = default_base_dir();
    let tantivy_dir = base_dir.join("sessions").join(session_id).join("tantivy");
    tantivy_dir.is_dir()
}

/// 为已有会话消息建立索引（回溯索引，批量写入后统一 commit）
pub fn backfill_session_index(
    session_id: &str,
    messages: &[tiangong_types::Message],
) -> Result<usize> {
    if messages.is_empty() {
        return Ok(0);
    }
    let turns: Vec<TurnData> = messages
        .iter()
        .filter_map(|msg| {
            let role = match msg.role {
                tiangong_types::MessageRole::User => "user",
                tiangong_types::MessageRole::Assistant => "assistant",
                tiangong_types::MessageRole::Tool => "tool",
                tiangong_types::MessageRole::System => return None,
            };
            let text = msg.text_content();
            if text.trim().is_empty() {
                return None;
            }
            Some(TurnData {
                turn_id: msg.id.clone(),
                workspace_id: String::new(),
                role: role.to_string(),
                content: text,
                topics: Vec::new(),
                entity_names: Vec::new(),
            })
        })
        .collect();
    let count = turns.len();
    if count == 0 {
        return Ok(0);
    }
    let manager = IndexManager::new()?;
    manager.index_turn_batch(session_id, &turns)?;
    manager.finalize_session_index(session_id)?;
    Ok(count)
}

fn workspace_index_dir(root: &Path) -> PathBuf {
    let base_dir = default_base_dir();
    let workspace_id = workspace_index::hash_path(root);
    base_dir
        .join("workspaces")
        .join(workspace_id)
        .join("tantivy")
}

fn default_base_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".tiangong").join("index")
}

/// 工作区在内存映射（`workspaces` / `scanning_roots`）中的统一 key。
///
/// 规范化路径以消除 `.`/`..`/软链接/相对路径导致的等价目录生成不同 key 问题，
/// 避免绕过扫描去重。canonicalize 失败（如路径暂不存在）时退回原始路径。
///
/// 注意：磁盘索引目录仍按 [`workspace_index::hash_path`] 的原始路径散列定位，
/// 以兼容已建索引；二者独立，不混用。
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
    ///
    /// 回归覆盖：原 `scanning_flag_for`「先 get 再 insert」非原子，多线程同时 miss
    /// 后各自创建不同 flag，导致同一工作区被并发扫描、状态观察失真。permit 方案下，
    /// `try_begin_workspace_scan` 的 `compare_exchange` 保证仅有一个调用方拿到许可。
    ///
    /// 注意：线程必须持有 permit 直到全部申请完成（再 join 计数）。若线程只返回
    /// `is_some()` 后立即 drop permit，标志会复位，下一个线程也能成功——那验证的是
    /// 「释放后可再取」而非并发去重。
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
            // 返回 permit 而非 bool，使 permit 存活至 join 之后，标志保持占用。
            handles.push(thread::spawn(move || {
                barrier.wait();
                mgr.try_begin_workspace_scan(&root)
            }));
        }
        // 收集所有 permit（存活到本块结束）后统计赢家数量。
        let permits: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let wins = permits.iter().filter(|p| p.is_some()).count();
        assert_eq!(wins, 1, "并发申请扫描许可时只应有一个成功，实际 {wins}");
        assert!(manager.is_workspace_scanning(&root));

        // 其它 root 不受影响。
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
        // 模拟扫描线程 panic：catch_unwind 确保 permit 在展开时被 drop。
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

        // 绝对路径取得许可。
        let _permit = manager
            .try_begin_workspace_scan(&root)
            .expect("取得扫描许可");

        // 「.」形式相对路径应规范化为同一 root，取许可应返回 None。
        let dot = root.join(".");
        assert!(
            manager.try_begin_workspace_scan(&dot).is_none(),
            "等价路径（.）应视为同一工作区，去重不被绕过"
        );
    }
}
