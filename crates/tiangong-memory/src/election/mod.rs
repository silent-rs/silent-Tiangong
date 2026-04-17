//! Leader 选举与进程注册（Phase B：进程内单 Leader 模式）
//!
//! Phase B 实现进程内 Leader 模式：使用文件锁（lock 文件）标记当前 Leader 进程。
//! Phase B+ 完善：UDS 心跳 + 多进程自动接替。

use std::path::PathBuf;

/// 进程类型（用于 Leader 选举，区分 GUI/CLI/Server）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessType {
    Gui,
    Cli,
    Server,
}

/// Leader 状态
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum LeaderState {
    /// 本进程是 Leader（持有文件锁）
    Leader,
    /// 本进程是 Follower（通过 IPC 访问 Leader）
    Follower { pid: u32 },
}

/// 获取 Leader 锁文件路径
#[allow(dead_code)]
pub(crate) fn leader_lock_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("memory")
        .join("leader.lock")
}

#[allow(dead_code)]
fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    None
}
