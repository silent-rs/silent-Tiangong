//! Desktop 运行检测锁（混合方案：Desktop/CLI 并发控制）。
//!
//! Desktop 启动时持 `~/.tiangong/desktop.lock` 独占锁，退出时释放。
//! CLI 写操作前检查此锁：Desktop 运行时拒绝，避免 CLI 停止 Desktop supervisor
//! 管理的 bot 后被自动拉起。

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use crate::io::storage_root;

/// Desktop 锁文件路径。
fn lock_path() -> PathBuf {
    storage_root().join("desktop.lock")
}

/// Desktop 是否正在运行（探测锁是否被持有）。
pub fn is_desktop_running() -> bool {
    let path = lock_path();
    let file = match OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return false,
    };
    match fs4::FileExt::try_lock_exclusive(&file) {
        Ok(()) => {
            let _ = fs4::FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

/// 尝试获取 Desktop 独占锁。成功返回持有句柄（随进程退出自动释放），失败返回 None。
pub fn acquire() -> Option<File> {
    let path = lock_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    if fs4::FileExt::try_lock_exclusive(&file).is_ok() {
        Some(file)
    } else {
        None
    }
}
