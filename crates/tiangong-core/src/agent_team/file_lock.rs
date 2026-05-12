use std::collections::HashMap;
use std::path::PathBuf;

/// 文件锁记录
#[derive(Debug, Clone)]
pub struct FileLock {
    /// 持有锁的 Agent ID
    pub holder: String,
    /// 锁获取时间
    pub locked_at: String,
}

/// 文件编辑锁管理器
pub struct FileLockManager {
    locks: HashMap<PathBuf, FileLock>,
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
        }
    }

    /// 尝试获取文件锁
    pub fn try_lock(&mut self, path: &PathBuf, agent_id: &str, now: &str) -> Result<(), String> {
        if let Some(lock) = self.locks.get(path) {
            if lock.holder != agent_id {
                return Err(format!(
                    "文件 {} 被 Agent {} 锁定（{}）",
                    path.display(),
                    lock.holder,
                    lock.locked_at
                ));
            }
            return Ok(());
        }
        self.locks.insert(
            path.clone(),
            FileLock {
                holder: agent_id.to_string(),
                locked_at: now.to_string(),
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

    /// 检查文件是否被锁定（且不是当前 Agent 持有）
    pub fn is_locked_by_other(&self, path: &PathBuf, agent_id: &str) -> bool {
        self.locks
            .get(path)
            .is_some_and(|lock| lock.holder != agent_id)
    }

    /// 释放指定 Agent 持有的所有锁
    pub fn release_all(&mut self, agent_id: &str) {
        self.locks.retain(|_, lock| lock.holder != agent_id);
    }

    /// 强制释放锁（主 Agent 权限）
    pub fn force_unlock(&mut self, path: &PathBuf) {
        self.locks.remove(path);
    }
}
