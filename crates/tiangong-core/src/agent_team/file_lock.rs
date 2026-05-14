use std::collections::HashMap;
use std::path::PathBuf;

/// 默认锁超时时间（秒）
const DEFAULT_LOCK_TIMEOUT_SECS: i64 = 300;

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
            timeout_secs: DEFAULT_LOCK_TIMEOUT_SECS,
        }
    }

    /// 尝试获取文件锁
    pub fn try_lock(
        &mut self,
        path: &PathBuf,
        agent_id: &str,
        now: &chrono::NaiveDateTime,
    ) -> Result<(), String> {
        if let Some(lock) = self.locks.get(path) {
            if lock.holder != agent_id {
                // 检查是否已超时，超时则自动释放
                if self.is_expired(&lock.locked_at, now) {
                    self.locks.remove(path);
                } else {
                    return Err(format!(
                        "文件 {} 被 Agent {} 锁定（{}）",
                        path.display(),
                        lock.holder,
                        lock.locked_at
                    ));
                }
            } else {
                // 同一 Agent 重复获取，刷新时间
                return Ok(());
            }
        }
        self.locks.insert(
            path.clone(),
            FileLock {
                holder: agent_id.to_string(),
                locked_at: *now,
            },
        );
        Ok(())
    }

    /// 释放文件锁
    pub fn unlock(&mut self, path: &PathBuf, agent_id: &str) -> Result<(), String> {
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
    pub fn holder(&self, path: &PathBuf) -> Option<&str> {
        self.locks.get(path).map(|lock| lock.holder.as_str())
    }

    /// 检查文件是否被其他 Agent 锁定，返回持有者 ID（已超时的锁视为未锁定）
    pub fn is_locked_by_other(
        &self,
        path: &PathBuf,
        agent_id: &str,
        now: &chrono::NaiveDateTime,
    ) -> Option<&str> {
        self.locks.get(path).and_then(|lock| {
            if lock.holder != agent_id && !self.is_expired(&lock.locked_at, now) {
                Some(lock.holder.as_str())
            } else {
                None
            }
        })
    }

    /// 检查当前 Agent 是否允许写入文件。
    ///
    /// 主 Agent 由调用方放行；Sub Agent 必须持有目标文件锁。
    pub fn ensure_can_write(
        &mut self,
        path: &PathBuf,
        agent_id: &str,
        now: &chrono::NaiveDateTime,
    ) -> Result<(), String> {
        if agent_id == "main" {
            return Ok(());
        }
        self.release_expired(now);
        match self.locks.get(path) {
            Some(lock) if lock.holder == agent_id => Ok(()),
            Some(lock) => Err(format!(
                "文件 {} 被 Agent {} 锁定（{}）",
                path.display(),
                lock.holder,
                lock.locked_at
            )),
            None => Err(format!(
                "文件 {} 尚未加锁，Sub Agent 编辑前必须先调用 lock_file",
                path.display()
            )),
        }
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
    pub fn force_unlock(&mut self, path: &PathBuf) {
        self.locks.remove(path);
    }

    /// 释放所有已超时的锁
    pub fn release_expired(&mut self, now: &chrono::NaiveDateTime) {
        let timeout = self.timeout_secs;
        self.locks
            .retain(|_, lock| !is_expired(&lock.locked_at, now, timeout));
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
