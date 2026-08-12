//! CoreManager：会话级 TiangongCore 注册表与资源管理层。
//!
//! 定位（见 issue #245）：
//! - 持有 `session_id -> TiangongCore` 映射，承接 app-state 收窄后剥离的「资源
//!   加载与管理」职责
//! - **不执行 turn**（turn 仍由 Core 内部驱动）
//! - session 真相源是磁盘，CoreManager 只做按需加载与缓存
//!
//! CoreManager 针对 `TiangongCore`，不抽象「其他 Core 类型」。Core 的实际构造
//! 内置在 `ensure_core`：host 在调用前构造好 plugin 集合并作为参数传入
//! （不同 host 的 plugin 构造差异大，不能在共享层硬编码）。

pub mod ensure;
pub mod registry;

pub use self::registry::{CoreRegistry, CoreRegistryGuard};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::CoreConfig;
use tiangong_core::session::Session;

use crate::SessionMetadata;

/// `ensure_core` 的返回：区分新建与复用既有 Core。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsuredCore {
    pub session_id: String,
    pub is_new: bool,
}

/// 会话级 TiangongCore 管理器。
///
/// `#[derive(Clone)]` 的廉价句柄，内部状态全经 `Arc<Mutex<>>` 共享。
#[derive(Clone)]
pub struct CoreManager {
    cores: Arc<Mutex<HashMap<String, TiangongCore>>>,
    /// 每会话的创建互斥锁：覆盖同一会话从「检查 Core 是否存在」到「插入新 Core」
    /// 的完整创建区间，防止两路并发为同一 session 各建一份 Core。
    ///
    /// 锁对象不主动删除，避免旧等待者尚未退出时为同一 session 创建第二把锁。
    creation_locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    config: tiangong_core::core_config::CoreConfigProvider,
    storage_root: PathBuf,
}

impl CoreManager {
    /// 构造管理器。
    ///
    /// - `config`：全局配置模板 provider，用于 `ensure_core` 的 base 快照与
    ///   `sync_config` 的模板替换
    /// - `storage_root`：session 文件根（形如 `~/.tiangong`）
    pub fn new(
        config: tiangong_core::core_config::CoreConfigProvider,
        storage_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cores: Arc::new(Mutex::new(HashMap::new())),
            creation_locks: Arc::new(Mutex::new(HashMap::new())),
            config,
            storage_root: storage_root.into(),
        }
    }

    /// 全局配置 provider（host 用它取 base 快照构建 per-session 配置）。
    pub fn config(&self) -> &tiangong_core::core_config::CoreConfigProvider {
        &self.config
    }

    /// 会话文件根目录。
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// 从磁盘按 id 加载完整 Session（委托 `Session::load_from_storage`）。
    pub fn load_session(&self, session_id: &str) -> Result<Session, String> {
        Session::load_from_storage(&self.storage_root, session_id)
    }

    /// 扫描磁盘 `sessions/` 目录，返回所有会话 id（issue #245：会话列表真相源归磁盘）。
    pub fn list_session_ids(&self) -> Vec<String> {
        let dir = self.storage_root.join("sessions");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            if path.extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                ids.push(stem.to_string());
            }
        }
        ids
    }

    /// 批量加载所有会话的元数据（浅字段，不构造完整 Session）。
    ///
    /// 会话文件损坏时跳过（记 warn），不阻断列表。
    pub fn list_session_metadata(&self) -> Vec<SessionMetadata> {
        self.list_session_ids()
            .iter()
            .filter_map(
                |id| match SessionMetadata::load_from_storage(&self.storage_root, id) {
                    Ok(meta) => Some(meta),
                    Err(error) => {
                        tracing::warn!(session_id = %id, %error, "跳过损坏的会话文件");
                        None
                    }
                },
            )
            .collect()
    }

    /// 指定会话在磁盘上是否存在。
    pub fn session_exists(&self, session_id: &str) -> bool {
        self.storage_root
            .join("sessions")
            .join(format!("{session_id}.json"))
            .exists()
    }

    /// 删除磁盘上的会话文件（不操作 cores registry——调用方按需 retire_core）。
    pub fn delete_session_file(&self, session_id: &str) -> Result<(), String> {
        let path = self
            .storage_root
            .join("sessions")
            .join(format!("{session_id}.json"));
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| format!("删除会话文件失败：{error}"))?;
        }
        Ok(())
    }

    /// 将会话文件原子移动到回收区 `trash/sessions/{id}.json`。
    ///
    /// 同文件系统 `fs::rename` 保证原子性：要么成功，要么文件原封不动。
    /// 移走后 `list_session_metadata()` 扫描 `sessions/` 天然看不到。
    /// 文件不存在时幂等返回 Ok。
    pub fn trash_session_file(&self, session_id: &str) -> Result<(), String> {
        let src = self
            .storage_root
            .join("sessions")
            .join(format!("{session_id}.json"));
        if !src.exists() {
            return Ok(());
        }
        let trash_dir = self.storage_root.join("trash").join("sessions");
        std::fs::create_dir_all(&trash_dir)
            .map_err(|error| format!("创建回收区目录失败：{error}"))?;
        let dst = trash_dir.join(format!("{session_id}.json"));
        std::fs::rename(&src, &dst).map_err(|error| {
            format!(
                "会话文件移动到回收区失败（{} → {}）：{error}",
                src.display(),
                dst.display()
            )
        })
    }

    /// 将会话从回收区恢复到 `sessions/`（原子 rename）。
    ///
    /// 文件不存在于回收区时返回错误。目标已存在同名文件时返回错误（避免覆盖）。
    pub fn restore_session_file(&self, session_id: &str) -> Result<(), String> {
        let _ = crate::session_file_path(&self.storage_root, session_id)?;
        let src = self
            .storage_root
            .join("trash")
            .join("sessions")
            .join(format!("{session_id}.json"));
        if !src.exists() {
            return Err(format!("回收区中不存在会话 {session_id}"));
        }
        let dst = self
            .storage_root
            .join("sessions")
            .join(format!("{session_id}.json"));
        if dst.exists() {
            return Err(format!("会话 {session_id} 已存在于正常目录，无法恢复"));
        }
        std::fs::rename(&src, &dst).map_err(|error| {
            format!(
                "恢复会话文件失败（{} → {}）：{error}",
                src.display(),
                dst.display()
            )
        })
    }

    /// 扫描回收区 `trash/sessions/`，返回残留会话 ID 列表。
    ///
    /// 这些是逻辑删除后等待物理清理的会话。
    pub fn list_trashed_session_ids(&self) -> Vec<String> {
        let trash_dir = self.storage_root.join("trash").join("sessions");
        let Ok(entries) = std::fs::read_dir(&trash_dir) else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            if path.extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                ids.push(stem.to_string());
            }
        }
        ids
    }

    /// 删除回收区中的会话文件（物理清理阶段调用）。
    pub fn delete_trashed_session(&self, session_id: &str) -> Result<(), String> {
        let path = self
            .storage_root
            .join("trash")
            .join("sessions")
            .join(format!("{session_id}.json"));
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("删除回收区会话文件失败：{error}"))?;
        }
        Ok(())
    }

    /// 逻辑删除会话：原子移动会话文件到回收区 + 取消 Core。
    ///
    /// 不做 retire_core 的 finalize（load/persist/plugins）——会话文件已移到 trash，
    /// 加载和保存无意义。不删媒体/teams（那些留给物理清理）。
    /// Core 不存在时只移文件。
    pub async fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let creation_lock = self.creation_lock(session_id);
        let _creation_guard = creation_lock.lock_owned().await;
        // 先移文件到回收区（原子操作，极快）。
        self.trash_session_file(session_id)?;
        // 取消活跃 turn 并摘除 Core（不等 worker 退出）。
        let _ = self.cancel_core(session_id);
        let _ = self.take_core(session_id);
        Ok(())
    }

    /// 同步配置快照到所有存活 Core。
    ///
    /// 对每个存活 Core 调用 `replace_config`，并即时同步会话级运行配置。host 负责在调用
    /// 前构建好 `session_id -> CoreConfig` 映射（通常读 app-state 的配置缓存）。
    /// 同时把 `template` 替换为全局 provider 的最新模板（仅作新建 Core 的辅助，
    /// 不承载任一会话的 trust/reasoning 覆盖）。
    pub fn sync_config(&self, template: CoreConfig, session_configs: &HashMap<String, CoreConfig>) {
        self.config.replace(template);
        let mut registry = self.registry();
        for (session_id, core) in registry.iter_mut() {
            if let Some(config) = session_configs.get(session_id) {
                let _ = core.replace_config(config.clone());
                core.set_trust_mode(config.trust_mode);
                core.set_reasoning_effort(config.reasoning_effort.clone());
            }
        }
    }

    /// 取得 registry 的句柄（暴露给 host 做 deliver / has_live 等只读访问）。
    pub fn registry(&self) -> CoreRegistryGuard<'_> {
        CoreRegistryGuard::lock(&self.cores)
    }

    /// 每会话的创建互斥锁句柄（供 host 取锁后串行化创建前后的额外工作）。
    pub fn creation_lock(&self, session_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = match self.creation_locks.lock() {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(error = %error, "Core 创建锁表已损坏，恢复后继续");
                error.into_inner()
            }
        };
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// 是否持有可投递的 Core。
    pub fn has_live_core(&self, session_id: &str) -> bool {
        self.registry().contains_key(session_id)
    }
}

impl std::fmt::Debug for CoreManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let live = self.cores.lock().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("CoreManager")
            .field("live_cores", &live)
            .field("storage_root", &self.storage_root)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
    use tiangong_core::session::Session;

    fn make_manager(dir: &tempfile::TempDir) -> CoreManager {
        CoreManager::new(
            CoreConfigProvider::new(CoreConfig::default()),
            dir.path().to_path_buf(),
        )
    }

    fn persist_session(dir: &tempfile::TempDir, id: &str) {
        let mut session = Session::new("test");
        session.id = id.to_string();
        session.bind_storage_root(dir.path().to_path_buf());
        session.try_persist_to_disk().unwrap();
    }

    #[test]
    fn trash_session_file_moves_to_trash() {
        let dir = tempfile::tempdir().unwrap();
        persist_session(&dir, "s1");
        let manager = make_manager(&dir);
        assert!(manager.session_exists("s1"));

        manager.trash_session_file("s1").unwrap();
        // 原位置不存在
        assert!(!manager.session_exists("s1"));
        // trash 中存在
        let trashed = manager.list_trashed_session_ids();
        assert_eq!(trashed, vec!["s1"]);
    }

    #[test]
    fn trash_session_file_idempotent_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let manager = make_manager(&dir);
        // 文件不存在时应幂等返回 Ok
        manager.trash_session_file("never-existed").unwrap();
    }

    #[test]
    fn delete_trashed_session_removes_from_trash() {
        let dir = tempfile::tempdir().unwrap();
        persist_session(&dir, "s2");
        let manager = make_manager(&dir);
        manager.trash_session_file("s2").unwrap();
        assert_eq!(manager.list_trashed_session_ids().len(), 1);

        manager.delete_trashed_session("s2").unwrap();
        assert!(manager.list_trashed_session_ids().is_empty());
    }

    #[test]
    fn list_trashed_empty_when_no_trash_dir() {
        let dir = tempfile::tempdir().unwrap();
        let manager = make_manager(&dir);
        assert!(manager.list_trashed_session_ids().is_empty());
    }

    #[tokio::test]
    async fn delete_session_trashes_file_and_removes_core() {
        let dir = tempfile::tempdir().unwrap();
        persist_session(&dir, "s3");
        let manager = make_manager(&dir);
        assert!(manager.session_exists("s3"));

        manager.delete_session("s3").await.unwrap();
        // 文件应移到 trash（不再在 sessions/ 中）
        assert!(!manager.session_exists("s3"));
        assert_eq!(manager.list_trashed_session_ids(), vec!["s3"]);
    }

    #[tokio::test]
    async fn delete_session_idempotent_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let manager = make_manager(&dir);
        // 文件不存在时 delete_session 不报错
        let result = manager.delete_session("never-existed").await;
        assert!(result.is_ok());
    }

    #[test]
    fn list_trashed_session_ids_lists_all_json() {
        let dir = tempfile::tempdir().unwrap();
        persist_session(&dir, "a");
        persist_session(&dir, "b");
        let manager = make_manager(&dir);
        manager.trash_session_file("a").unwrap();
        manager.trash_session_file("b").unwrap();
        let mut ids = manager.list_trashed_session_ids();
        ids.sort();
        assert_eq!(ids, vec!["a", "b"]);
    }
}
