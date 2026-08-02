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

    /// 删除会话：先 retire 对应 Core（取消在途 turn + 等待写盘），
    /// 再删除磁盘 session 文件（issue #245）。
    ///
    /// 这是安全的操作——retire_core 保证 worker 停止并写盘结束后才返回，
    /// 随后删除文件不会影响在途 turn。Core 不存在时只删文件。
    pub async fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let creation_lock = self.creation_lock(session_id);
        let _creation_guard = creation_lock.lock_owned().await;
        self.retire_core_locked(session_id, true).await?;
        self.delete_session_file(session_id)
    }

    /// 同步配置快照到所有存活 Core。
    ///
    /// 对每个存活 Core 调用 `replace_config` + `set_trust_mode`。host 负责在调用
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
