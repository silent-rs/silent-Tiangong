use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::constants::FILE_LOCK_LEASE_SECS;

/// 文件锁记录
#[derive(Debug, Clone)]
pub struct FileLock {
    /// 持有锁的 Agent ID
    pub holder: String,
    /// 锁获取时间
    pub locked_at: chrono::NaiveDateTime,
}

/// 文件编辑锁管理器
pub struct FileLockManager {
    locks: HashMap<PathBuf, FileLock>,
    /// 超时时间（秒）
    timeout_secs: i64,
}

impl Default for FileLockManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FileLockManager {
    pub fn new() -> Self {
        Self {
            locks: HashMap::new(),
            timeout_secs: FILE_LOCK_LEASE_SECS,
        }
    }

    /// 尝试获取文件锁
    pub fn try_lock(
        &mut self,
        path: &Path,
        agent_id: &str,
        now: &chrono::NaiveDateTime,
    ) -> Result<(), String> {
        let _ = self.release_expired(now);
        if let Some((locked_path, lock)) = self
            .locks
            .iter()
            .find(|(locked_path, lock)| paths_overlap(locked_path, path) && lock.holder != agent_id)
        {
            return Err(format!(
                "路径 {} 与 Agent {} 持有的锁 {} 冲突（{}）",
                path.display(),
                lock.holder,
                locked_path.display(),
                lock.locked_at
            ));
        }
        if let Some(covering_path) = self
            .locks
            .iter()
            .find(|(locked_path, lock)| lock.holder == agent_id && path.starts_with(locked_path))
            .map(|(locked_path, _)| locked_path.clone())
        {
            if let Some(lock) = self.locks.get_mut(&covering_path) {
                lock.locked_at = *now;
            }
            return Ok(());
        }
        self.locks.insert(
            path.to_path_buf(),
            FileLock {
                holder: agent_id.to_string(),
                locked_at: *now,
            },
        );
        Ok(())
    }

    /// 取出指定路径上已经超时的锁，供调用方发送状态变更事件。
    pub fn take_expired(&mut self, path: &Path, now: &chrono::NaiveDateTime) -> Option<FileLock> {
        let expired = self
            .locks
            .get(path)
            .is_some_and(|lock| self.is_expired(&lock.locked_at, now));
        expired.then(|| self.locks.remove(path)).flatten()
    }

    /// 释放文件锁
    pub fn unlock(&mut self, path: &Path, agent_id: &str) -> Result<(), String> {
        if let Some(lock) = self.locks.get(path) {
            if lock.holder != agent_id {
                return Err(format!(
                    "无法释放 {} 的锁：持有者是 {}",
                    path.display(),
                    lock.holder
                ));
            }
            self.locks.remove(path);
            Ok(())
        } else {
            Ok(())
        }
    }

    /// 获取指定文件当前锁持有者。
    pub fn holder(&self, path: &Path) -> Option<&str> {
        self.locks.get(path).map(|lock| lock.holder.as_str())
    }

    /// 检查文件是否被其他 Agent 锁定，返回持有者 ID（已超时的锁视为未锁定）
    pub fn is_locked_by_other(
        &self,
        path: &Path,
        agent_id: &str,
        now: &chrono::NaiveDateTime,
    ) -> Option<&str> {
        self.locks
            .iter()
            .find(|(locked_path, lock)| {
                paths_overlap(locked_path, path)
                    && lock.holder != agent_id
                    && !self.is_expired(&lock.locked_at, now)
            })
            .map(|(_, lock)| lock.holder.as_str())
    }

    /// 检查当前 Agent 是否允许写入文件。
    ///
    /// 主 Agent 由调用方放行；Sub Agent 必须持有目标文件锁。
    pub fn ensure_can_write(
        &mut self,
        path: &Path,
        agent_id: &str,
        now: &chrono::NaiveDateTime,
    ) -> Result<(), String> {
        if agent_id == "main" {
            return Ok(());
        }
        let _ = self.release_expired(now);
        if let Some((locked_path, lock)) = self
            .locks
            .iter()
            .find(|(locked_path, lock)| paths_overlap(locked_path, path) && lock.holder != agent_id)
        {
            return Err(format!(
                "路径 {} 与 Agent {} 持有的锁 {} 冲突（{}）",
                path.display(),
                lock.holder,
                locked_path.display(),
                lock.locked_at
            ));
        }
        let covering_path = self
            .locks
            .iter()
            .find(|(locked_path, lock)| lock.holder == agent_id && path.starts_with(locked_path))
            .map(|(locked_path, _)| locked_path.clone());
        if let Some(covering_path) = covering_path {
            // 每次真正开始写工具前续租，避免锁在命令刚启动时就接近过期。
            if let Some(lock) = self.locks.get_mut(&covering_path) {
                lock.locked_at = *now;
            }
            return Ok(());
        }
        Err(format!(
            "路径 {} 尚未加锁，Sub Agent 修改前必须先调用 lock_file",
            path.display()
        ))
    }

    /// 释放指定 Agent 持有的所有锁
    pub fn release_all(&mut self, agent_id: &str) -> Vec<String> {
        let released = self
            .locks
            .iter()
            .filter(|(_, lock)| lock.holder == agent_id)
            .map(|(path, _)| path.display().to_string())
            .collect::<Vec<_>>();
        self.locks.retain(|_, lock| lock.holder != agent_id);
        released
    }

    /// 强制释放锁（主 Agent 权限）
    pub fn force_unlock(&mut self, path: &Path) {
        self.locks.remove(path);
    }

    /// 释放所有已超时的锁
    pub fn release_expired(&mut self, now: &chrono::NaiveDateTime) -> Vec<(PathBuf, FileLock)> {
        let timeout = self.timeout_secs;
        let expired = self
            .locks
            .iter()
            .filter(|(_, lock)| is_expired(&lock.locked_at, now, timeout))
            .map(|(path, lock)| (path.clone(), lock.clone()))
            .collect::<Vec<_>>();
        for (path, _) in &expired {
            self.locks.remove(path);
        }
        expired
    }

    /// 获取当前所有活跃锁的摘要信息
    pub fn active_locks_summary(&self) -> Vec<(String, String, chrono::NaiveDateTime)> {
        self.locks
            .iter()
            .map(|(path, lock)| {
                (
                    path.display().to_string(),
                    lock.holder.clone(),
                    lock.locked_at,
                )
            })
            .collect()
    }

    fn is_expired(&self, locked_at: &chrono::NaiveDateTime, now: &chrono::NaiveDateTime) -> bool {
        is_expired(locked_at, now, self.timeout_secs)
    }
}

fn is_expired(
    locked_at: &chrono::NaiveDateTime,
    now: &chrono::NaiveDateTime,
    timeout_secs: i64,
) -> bool {
    (*now - *locked_at).num_seconds() > timeout_secs
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, NaiveDate};

    use super::*;

    fn instant() -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 12)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    #[test]
    fn repeated_lock_refreshes_timeout_and_expiry_can_be_reported() {
        let mut locks = FileLockManager::new();
        let path = PathBuf::from("src/lib.rs");
        let started = instant();
        locks.try_lock(&path, "dev", &started).unwrap();

        let refreshed = started + Duration::seconds(240);
        locks.try_lock(&path, "dev", &refreshed).unwrap();
        assert!(locks
            .take_expired(&path, &(started + Duration::seconds(301)))
            .is_none());

        let expired = locks
            .take_expired(&path, &(refreshed + Duration::seconds(301)))
            .expect("refreshed lock should eventually expire");
        assert_eq!(expired.holder, "dev");
        assert!(locks.holder(&path).is_none());
    }

    #[test]
    fn directory_lock_covers_descendants_and_conflicts_with_file_locks() {
        let mut locks = FileLockManager::new();
        let workspace = PathBuf::from("/workspace");
        let file = workspace.join("src/lib.rs");
        let now = instant();

        locks.try_lock(&workspace, "dev", &now).unwrap();
        locks.ensure_can_write(&file, "dev", &now).unwrap();
        assert!(locks.try_lock(&file, "test", &now).is_err());

        locks.unlock(&workspace, "dev").unwrap();
        locks.try_lock(&file, "dev", &now).unwrap();
        assert!(locks.try_lock(&workspace, "test", &now).is_err());
    }

    #[test]
    fn write_guard_refreshes_the_covering_lock_lease() {
        let mut locks = FileLockManager::new();
        let workspace = PathBuf::from("/workspace");
        let file = workspace.join("src/lib.rs");
        let started = instant();
        locks.try_lock(&workspace, "dev", &started).unwrap();

        let refreshed = started + Duration::seconds(240);
        locks.ensure_can_write(&file, "dev", &refreshed).unwrap();

        assert!(locks
            .take_expired(&workspace, &(started + Duration::seconds(301)))
            .is_none());
        assert!(locks
            .take_expired(&workspace, &(refreshed + Duration::seconds(301)))
            .is_some());
    }
}
