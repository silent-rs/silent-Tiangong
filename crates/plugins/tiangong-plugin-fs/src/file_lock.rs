//! 文件编辑锁：进程级共享锁表。
//!
//! 一份进程内全局的「文件路径锁表」，以规范化后的绝对路径为唯一标识，
//! 跨所有 [`crate::plugin::FsPlugin`] 实例共享（主 Agent 与每个子 Agent 各
//! 持独立插件实例，但共享同一份锁表）。
//!
//! ## 语义
//!
//! - **工具调用级**：fs 写工具（`write_file` / `replace_in_file` /
//!   `apply_patch`）执行前对目标路径加锁、执行后释放，对模型完全透明。
//! - **不区分调用方**：文件只要已有锁，任何后续写操作都拒绝——不区分是否
//!   来自同一 Agent。防止并发写同一文件互相覆盖。
//! - **全有或全无**：`apply_patch` 一次可能涉及多个文件，任一被占用则本次
//!   操作全部不加锁。
//! - **租约兜底**：锁持有方异常卡死时，过期锁（默认 300s）在下次加锁时
//!   静默清理，新操作可重新取得锁。
//! - **操作编号防误删**：旧操作超时后新操作可能取得锁；旧操作随后结束时，
//!   靠 `operation_id` 校验，不会误删新操作的锁。
//!
//! ## 限制
//!
//! 只保护同一个天工进程内的写入。多进程同时打开同一工作区时互斥需要系统级
//! 文件锁，不在本模块职责范围内。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 锁租约（秒）：超过该时长未被释放的锁视为过期，下次加锁时静默清理。
pub(crate) const FILE_LOCK_LEASE_SECS: i64 = 300;

/// 单条锁记录：只保存获取时间与本次操作的唯一编号。
#[derive(Debug, Clone)]
struct LockRecord {
    acquired_at: chrono::NaiveDateTime,
    operation_id: String,
}

/// 进程级共享锁表句柄。
///
/// 所有 `FsPlugin` 实例通过 [`FileLockTable::shared`] 获取同一份全局表。
/// 该类型本身零开销（内部 `OnceLock` 懒初始化），可作为 `FsPlugin` 的
/// 零字段存在，或直接调用静态方法。
pub(crate) struct FileLockTable;

impl FileLockTable {
    /// 取进程内唯一的锁表（懒初始化）。
    fn shared() -> &'static Mutex<HashMap<PathBuf, LockRecord>> {
        static TABLE: OnceLock<Mutex<HashMap<PathBuf, LockRecord>>> = OnceLock::new();
        TABLE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// 尝试对一组路径原子加锁（全有或全无）。
    ///
    /// 成功返回本次操作的 `operation_id`（调用方需保存，用于后续 [`release`]
    /// 校验）；任一路径已被占用（且未过期）则全部不加锁，返回错误文案。
    /// 加锁过程中会先清理本次涉及路径上的过期记录。
    pub(crate) fn acquire(
        paths: &[PathBuf],
        now: &chrono::NaiveDateTime,
    ) -> Result<String, String> {
        let mut table = Self::shared()
            .lock()
            .map_err(|_| "文件锁状态锁定失败".to_string())?;

        // 先去重 + 清理本次涉及路径上的过期记录。
        let unique = dedup_paths(paths);
        purge_expired_in(&mut table, &unique, now);

        // 任一路径已被占用则整体失败。
        if let Some(path) = unique.iter().find(|p| table.contains_key(*p)) {
            return Err(format!("文件 {} 正被其他写操作占用", path.display()));
        }

        let operation_id = scru128::new().to_string();
        for path in &unique {
            table.insert(
                path.clone(),
                LockRecord {
                    acquired_at: *now,
                    operation_id: operation_id.clone(),
                },
            );
        }
        Ok(operation_id)
    }

    /// 释放一组路径上由 `operation_id` 取得的锁。
    ///
    /// 仅删除 `operation_id` 匹配的记录——若某路径已被新操作接管（旧操作
    /// 超时后），则该记录的 `operation_id` 不同，不会被误删。holder 不匹配
    /// 等异常静默忽略（租约兜底）。
    pub(crate) fn release(paths: &[PathBuf], operation_id: &str) {
        let Ok(mut table) = Self::shared().lock() else {
            return;
        };
        for path in dedup_paths(paths) {
            if let Some(record) = table.get(&path)
                && record.operation_id == operation_id
            {
                table.remove(&path);
            }
        }
    }
}

/// 对一组路径去重（保留顺序，按值相等去重）。
fn dedup_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .iter()
        .filter(|p| seen.insert((*p).clone()))
        .cloned()
        .collect()
}

/// 清理指定路径集合中已过期的记录（惰性清理）。
fn purge_expired_in(
    table: &mut HashMap<PathBuf, LockRecord>,
    paths: &[PathBuf],
    now: &chrono::NaiveDateTime,
) {
    for path in paths {
        if let Some(record) = table.get(path)
            && is_expired(&record.acquired_at, now)
        {
            table.remove(path);
        }
    }
}

fn is_expired(locked_at: &chrono::NaiveDateTime, now: &chrono::NaiveDateTime) -> bool {
    (*now - *locked_at).num_seconds() > FILE_LOCK_LEASE_SECS
}

/// 把任意路径规范化为锁表可用的稳定 key。
///
/// - 已存在的路径：直接 `canonicalize`（解析软链接到真实路径）。
/// - 不存在的路径（如 `apply_patch` 的 add 分支）：向上查找最近的已存在父
///   目录 `canonicalize`，再拼回剩余的不存在部分。这样通过软链接访问与
///   通过真实路径访问会得到同一个 key，避免重复加锁。
///
/// `canonicalize` 完全失败时（连父目录都不存在）回退原路径。
pub(crate) fn canonicalize_for_lock(path: &Path) -> PathBuf {
    // 已存在：直接 canonicalize。
    if path.exists() {
        return path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    }

    // 不存在：向上找最近的已存在祖先。
    let mut existing = path.parent();
    while let Some(ancestor) = existing
        && !ancestor.exists()
    {
        existing = ancestor.parent();
    }
    let Some(ancestor) = existing else {
        return path.to_path_buf();
    };
    let canonical_ancestor = match ancestor.canonicalize() {
        Ok(c) => c,
        Err(_) => return path.to_path_buf(),
    };
    // 拼回剩余的不存在部分。
    let remaining = path.strip_prefix(ancestor).unwrap_or(path);
    canonical_ancestor.join(remaining)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, NaiveDate};

    use super::*;

    fn instant() -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 21)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    #[test]
    fn acquire_releases_single_path() {
        let now = instant();
        let path = PathBuf::from("/tmp/fs-lock-test-single.txt");
        // 测试隔离：先确保无残留。
        FileLockTable::release(std::slice::from_ref(&path), "stale");

        let op = FileLockTable::acquire(std::slice::from_ref(&path), &now).unwrap();
        // 同一路径再次加锁应失败。
        let err = FileLockTable::acquire(std::slice::from_ref(&path), &now).unwrap_err();
        assert!(err.contains("正被其他写操作占用"), "{err}");

        FileLockTable::release(std::slice::from_ref(&path), &op);
        // 释放后可重新加锁。
        let op2 = FileLockTable::acquire(std::slice::from_ref(&path), &now).unwrap();
        FileLockTable::release(std::slice::from_ref(&path), &op2);
    }

    #[test]
    fn acquire_is_all_or_nothing() {
        let now = instant();
        let a = PathBuf::from("/tmp/fs-lock-test-all-a.txt");
        let b = PathBuf::from("/tmp/fs-lock-test-all-b.txt");
        FileLockTable::release(&[a.clone(), b.clone()], "stale");

        // 先占住 a。
        let op_hold = FileLockTable::acquire(std::slice::from_ref(&a), &now).unwrap();

        // 再尝试同时锁 a+b，应整体失败，b 不应被加锁。
        let err = FileLockTable::acquire(&[a.clone(), b.clone()], &now).unwrap_err();
        assert!(err.contains("正被其他写操作占用"));

        // 释放后重试 a+b 应成功。
        FileLockTable::release(std::slice::from_ref(&a), &op_hold);
        let op_both = FileLockTable::acquire(&[a.clone(), b.clone()], &now).unwrap();
        FileLockTable::release(&[a.clone(), b.clone()], &op_both);
    }

    #[test]
    fn expired_lock_can_be_reacquired() {
        let now = instant();
        let path = PathBuf::from("/tmp/fs-lock-test-expired.txt");
        FileLockTable::release(std::slice::from_ref(&path), "stale");

        let op_old = FileLockTable::acquire(std::slice::from_ref(&path), &now).unwrap();

        // 未过期时仍被占用。
        assert!(
            FileLockTable::acquire(std::slice::from_ref(&path), &(now + Duration::seconds(100)))
                .is_err()
        );

        // 过期后（>300s）新操作可取得锁。
        let later = now + Duration::seconds(301);
        let op_new = FileLockTable::acquire(std::slice::from_ref(&path), &later).unwrap();

        // 旧操作结束后不应误删新操作的锁。
        FileLockTable::release(std::slice::from_ref(&path), &op_old);
        // 新操作仍在，第三次加锁应失败。
        assert!(FileLockTable::acquire(std::slice::from_ref(&path), &later).is_err());

        FileLockTable::release(std::slice::from_ref(&path), &op_new);
    }

    #[test]
    fn canonicalize_unifies_symlink_and_real_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, "x").unwrap();
        let link = dir.path().join("link.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(not(unix))]
        let link = real.clone(); // 非 unix 无法测软链接，退化为同路径。

        let key_real = canonicalize_for_lock(&real);
        let key_link = canonicalize_for_lock(&link);
        assert_eq!(key_real, key_link, "软链接与真实路径应归一为同一锁 key");
    }

    #[test]
    fn canonicalize_handles_nonexistent_path() {
        let dir = tempfile::TempDir::new().unwrap();
        // 父目录存在、文件不存在的路径。
        let missing = dir.path().join("sub").join("missing.txt");
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();

        let key = canonicalize_for_lock(&missing);
        // key 应以已存在父目录的 canonical 路径为前缀。
        let canonical_parent = dir.path().canonicalize().unwrap();
        assert!(
            key.starts_with(&canonical_parent),
            "key {key:?} 应以已存在父目录 canonical 路径 {:?} 为前缀",
            canonical_parent
        );
        assert!(key.ends_with("sub/missing.txt"));
    }

    #[test]
    fn same_agent_concurrent_write_is_rejected() {
        // 锁不区分调用方：同一「Agent」连续两次加锁同一路径，第二次也应被拒。
        let now = instant();
        let path = PathBuf::from("/tmp/fs-lock-test-same-agent.txt");
        FileLockTable::release(std::slice::from_ref(&path), "stale");

        let op = FileLockTable::acquire(std::slice::from_ref(&path), &now).unwrap();
        let err = FileLockTable::acquire(std::slice::from_ref(&path), &now).unwrap_err();
        assert!(err.contains("正被其他写操作占用"));
        FileLockTable::release(std::slice::from_ref(&path), &op);
    }
}
