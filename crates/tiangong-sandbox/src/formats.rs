//! 快照与恢复层的数据结构。
//!
//! 快照按会话组织，落盘布局：
//!
//! ```text
//! ~/.tiangong/snapshots/<session_id>/
//!   index.json                 # 摘要列表（按时间升序）
//!   <snapshot_id>/meta.json    # 完整指纹表
//!   <snapshot_id>/tree/        # 文件树（相对路径保持，硬链接复用增量条目）
//!   orphans/<snapshot_id>/     # 回滚时被移出工作区的文件暂存
//! ```

use serde::{Deserialize, Serialize};

/// 快照触发原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotReason {
    /// turn 结束的常规快照。
    Turn,
    /// 回滚前自动拍摄的保护快照（保证回滚本身可撤销）。
    PreRestore,
    /// 手动触发。
    Manual,
}

/// 普通文件指纹：路径 + 大小 + 修改时间（纳秒）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// 相对工作区的路径，统一使用 `/` 分隔。
    pub rel_path: String,
    pub size: u64,
    pub mtime_ns: i64,
}

/// 符号链接条目（快照内不存实体，回滚时按记录重建）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymlinkEntry {
    pub rel_path: String,
    /// 链接目标（原样字符串）。
    pub target: String,
}

/// 单个快照的完整元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub id: String,
    pub session_id: String,
    pub turn_start_idx: usize,
    /// 本地时间（RFC 3339 格式的 naive 本地时间）。
    pub created_at: String,
    pub reason: SnapshotReason,
    pub files: Vec<FileEntry>,
    pub symlinks: Vec<SymlinkEntry>,
    pub file_count: u64,
    pub total_size: u64,
    /// 增量复用（硬链接自上一快照）的文件数。
    pub reused: u64,
    /// 本次实际拷贝的文件数。
    pub copied: u64,
}

/// index.json 中的摘要条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSummary {
    pub id: String,
    pub turn_start_idx: usize,
    pub created_at: String,
    pub reason: SnapshotReason,
    pub file_count: u64,
    pub total_size: u64,
}

impl From<&SnapshotMeta> for SnapshotSummary {
    fn from(meta: &SnapshotMeta) -> Self {
        Self {
            id: meta.id.clone(),
            turn_start_idx: meta.turn_start_idx,
            created_at: meta.created_at.clone(),
            reason: meta.reason,
            file_count: meta.file_count,
            total_size: meta.total_size,
        }
    }
}

/// 变更集条目类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
}

/// 变更集条目（快照与工作区或两快照之间的差异）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub kind: FileChangeKind,
    pub rel_path: String,
    pub size: u64,
}

/// 回滚报告。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RestoreReport {
    /// 从快照拷回工作区的文件数。
    pub restored_files: u64,
    /// 重建的符号链接数。
    pub restored_symlinks: u64,
    /// 工作区中存在但快照中没有、被移入暂存区的文件数。
    pub orphaned_files: u64,
    /// 回滚前保护快照的 id。
    pub protected_snapshot_id: Option<String>,
}

/// 会话索引（index.json）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionIndex {
    /// 按拍摄时间升序。
    pub snapshots: Vec<SnapshotSummary>,
}
