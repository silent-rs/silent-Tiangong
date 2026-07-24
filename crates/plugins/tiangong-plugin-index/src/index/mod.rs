use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    base_dir: PathBuf,
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
            base_dir,
        })
    }

    pub fn get_or_create_workspace_index(
        &self,
        root: &Path,
    ) -> Result<Arc<std::sync::Mutex<WorkspaceIndex>>> {
        let key = root.to_string_lossy().to_string();
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

pub fn rebuild_workspace_index_for_gui(root: &Path) -> Result<usize> {
    let manager = IndexManager::new()?;
    manager.rebuild_workspace_index(root)
}

/// 预热工作区索引：若索引已存在则直接返回，否则后台启动全量扫描，立即返回不阻塞。
///
/// 用于在创建新对话/选定工作目录时提前构建索引，消除首次发送消息时的同步扫描延迟。
/// 线程内结果仅记日志（info 成功 / warn 失败），不影响调用方。
pub fn prewarm_workspace_index_for_gui(root: &Path) -> Result<()> {
    if !root.is_dir() || workspace_index_exists(root) {
        return Ok(());
    }
    let manager = IndexManager::new()?;
    let root = root.to_path_buf();
    tracing::info!(workspace = %root.display(), "Workspace 索引预热启动");
    std::thread::spawn(move || match manager.full_scan(&root) {
        Ok(count) => tracing::info!(count, "Workspace 索引预热完成"),
        Err(e) => tracing::warn!("Workspace 索引预热失败: {e}"),
    });
    Ok(())
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
