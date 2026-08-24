//! 快照引擎：扫描、增量快照、变更集与回滚。
//!
//! 每个会话独立目录；快照之间通过指纹（路径 + 大小 + mtime 纳秒）做增量：
//! 未变化的文件在快照区内部以硬链接复用（快照区条目创建后绝不原地修改，
//! 共享 inode 安全）；变化文件走平台写时复制（clonefile / FICLONE / copy）。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, anyhow, bail};

use crate::copy::{self, atomic_write};
use crate::formats::{
    FileChange, FileChangeKind, FileEntry, RestoreReport, SessionIndex, SnapshotMeta,
    SnapshotReason, SnapshotSummary, SymlinkEntry,
};

/// 引擎配置。v1 使用默认值，后续接入设置页后可调。
#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    /// 每会话保留的最大快照数，超出淘汰最旧。
    pub max_snapshots_per_session: usize,
    /// 每会话快照区容量上限（字节，按 meta 记录的 total_size 累计），超出淘汰最旧。
    pub max_total_bytes_per_session: u64,
    /// 单快照文件数超过该值时记录告警（疑似勒索式批量改写）。
    pub warn_file_count: u64,
    /// 忽略名单：目录名或文件名精确匹配（不快照，体积大且可再生）。
    pub ignore_names: Vec<String>,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            max_snapshots_per_session: 50,
            max_total_bytes_per_session: 5 * 1024 * 1024 * 1024,
            warn_file_count: 20_000,
            ignore_names: [
                "node_modules",
                "target",
                "dist",
                "build",
                "out",
                ".next",
                ".cache",
                "__pycache__",
                ".git",
                ".DS_Store",
                ".venv",
                "venv",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }
    }
}

pub struct SnapshotEngine {
    /// 快照根目录（<storage>/snapshots）。
    root: PathBuf,
    config: SnapshotConfig,
    /// session_id -> 最近一次快照的工作区路径（恢复与变更集使用）。
    workspaces: HashMap<String, PathBuf>,
}

impl SnapshotEngine {
    pub fn new(root: impl Into<PathBuf>, config: SnapshotConfig) -> Self {
        Self {
            root: root.into(),
            config,
            workspaces: HashMap::new(),
        }
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id)
    }

    fn snapshot_dir(&self, session_id: &str, snapshot_id: &str) -> PathBuf {
        self.session_dir(session_id).join(snapshot_id)
    }

    fn tree_dir(&self, session_id: &str, snapshot_id: &str) -> PathBuf {
        self.snapshot_dir(session_id, snapshot_id).join("tree")
    }

    fn meta_path(&self, session_id: &str, snapshot_id: &str) -> PathBuf {
        self.snapshot_dir(session_id, snapshot_id).join("meta.json")
    }

    fn index_path(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("index.json")
    }

    fn orphan_dir(&self, session_id: &str, snapshot_id: &str) -> PathBuf {
        self.session_dir(session_id)
            .join("orphans")
            .join(snapshot_id)
    }

    fn read_index(&self, session_id: &str) -> SessionIndex {
        fs::read_to_string(self.index_path(session_id))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn write_index(&self, session_id: &str, index: &SessionIndex) -> Result<()> {
        let path = self.index_path(session_id);
        let text = serde_json::to_vec_pretty(index).context("序列化会话索引失败")?;
        atomic_write(&path, &text).with_context(|| path.display().to_string())?;
        Ok(())
    }

    fn load_meta(&self, session_id: &str, snapshot_id: &str) -> Result<SnapshotMeta> {
        let path = self.meta_path(session_id, snapshot_id);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("读取快照元数据 {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| path.display().to_string())
    }

    fn save_meta(&self, meta: &SnapshotMeta) -> Result<()> {
        let path = self.meta_path(&meta.session_id, &meta.id);
        let text = serde_json::to_vec_pretty(meta).context("序列化快照元数据失败")?;
        atomic_write(&path, &text).with_context(|| path.display().to_string())?;
        Ok(())
    }

    /// 最近一次快照的完整元数据。
    pub fn latest_snapshot(&self, session_id: &str) -> Option<SnapshotMeta> {
        let index = self.read_index(session_id);
        index
            .snapshots
            .last()
            .and_then(|summary| self.load_meta(session_id, &summary.id).ok())
    }

    /// 快照摘要列表（按拍摄时间升序）。
    pub fn list_snapshots(&self, session_id: &str) -> Vec<SnapshotSummary> {
        self.read_index(session_id).snapshots
    }

    /// 拍摄快照：扫描工作区 → 与最近快照做增量 → 落盘 → 保留策略。
    pub fn take_snapshot(
        &mut self,
        session_id: &str,
        workspace: &Path,
        turn_start_idx: usize,
        reason: SnapshotReason,
    ) -> Result<SnapshotMeta> {
        if !workspace.is_dir() {
            bail!("工作区目录不存在：{}", workspace.display());
        }
        ensure_safe_id(session_id, "会话 ID")?;
        let snapshot_id = scru128::new().to_string();
        let tree_dir = self.tree_dir(session_id, &snapshot_id);
        fs::create_dir_all(&tree_dir)
            .with_context(|| format!("创建快照目录 {} 失败", tree_dir.display()))?;

        let (mut files, symlinks) = self
            .scan_workspace(workspace)
            .with_context(|| format!("扫描工作区 {} 失败", workspace.display()))?;
        files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

        let prev = self.latest_snapshot(session_id);
        let prev_tree = prev.as_ref().map(|m| self.tree_dir(session_id, &m.id));
        let prev_map = fingerprint_map(prev.as_ref());

        let mut reused: u64 = 0;
        let mut copied: u64 = 0;
        for entry in &files {
            let dst = tree_dir.join(&entry.rel_path);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
            }
            // 指纹一致：优先硬链接复用上一快照条目；复用失败回退从工作区拷贝。
            let unchanged = prev_map
                .get(&entry.rel_path)
                .is_some_and(|(size, mtime)| *size == entry.size && *mtime == entry.mtime_ns);
            if unchanged && let Some(tree) = prev_tree.as_deref() {
                let prev_file = tree.join(&entry.rel_path);
                if copy::link_or_copy(&prev_file, &dst).is_ok() {
                    reused += 1;
                    continue;
                }
            }
            let src = workspace.join(&entry.rel_path);
            copy::copy_file(&src, &dst).with_context(|| format!("拷贝 {} 失败", entry.rel_path))?;
            copied += 1;
        }

        let total_size: u64 = files.iter().map(|f| f.size).sum();
        let file_count = files.len() as u64;
        if file_count > self.config.warn_file_count {
            tracing::warn!(
                session_id,
                file_count,
                "快照文件数超过告警阈值，疑似异常批量改写"
            );
        }

        let meta = SnapshotMeta {
            id: snapshot_id,
            session_id: session_id.to_string(),
            turn_start_idx,
            created_at: chrono::Local::now()
                .naive_local()
                .format("%Y-%m-%dT%H:%M:%S%.3f")
                .to_string(),
            reason,
            files,
            symlinks,
            file_count,
            total_size,
            reused,
            copied,
        };
        self.save_meta(&meta)?;

        // 更新索引与保留策略（workspace 一并持久化，重启后恢复链路可用）。
        self.workspaces
            .insert(session_id.to_string(), workspace.to_path_buf());
        let mut index = self.read_index(session_id);
        index.workspace = Some(workspace.display().to_string());
        index.snapshots.push(SnapshotSummary::from(&meta));
        self.enforce_retention(session_id, &mut index);
        self.write_index(session_id, &index)?;
        Ok(meta)
    }

    /// 工作区当前状态与指定快照之间的差异（快照之后发生了什么）。
    pub fn changeset_vs_workspace(
        &self,
        session_id: &str,
        snapshot_id: &str,
        workspace: &Path,
    ) -> Result<Vec<FileChange>> {
        ensure_safe_id(session_id, "会话 ID")?;
        ensure_safe_id(snapshot_id, "快照 ID")?;
        let meta = self.load_meta(session_id, snapshot_id)?;
        let snap_files = fingerprint_map(Some(&meta));
        let snap_links: HashMap<String, String> = meta
            .symlinks
            .iter()
            .map(|l| (l.rel_path.clone(), l.target.clone()))
            .collect();

        let (ws_files, ws_links) = self.scan_workspace(workspace)?;
        let ws_map: HashMap<String, (u64, i64)> = ws_files
            .into_iter()
            .map(|f| (f.rel_path, (f.size, f.mtime_ns)))
            .collect();
        let ws_link_map: HashMap<String, String> = ws_links
            .into_iter()
            .map(|l| (l.rel_path, l.target))
            .collect();

        let mut changes = Vec::new();
        let mut all_paths: HashSet<&String> = HashSet::new();
        all_paths.extend(snap_files.keys());
        all_paths.extend(ws_map.keys());
        for rel in all_paths {
            match (snap_files.get(rel), ws_map.get(rel)) {
                (Some((size, mtime)), Some((wsize, wmtime)))
                    if size == wsize && mtime == wmtime =>
                {
                    // 大小与修改时间均相同视为未变化；等长改写（如 true→null）
                    // 经 mtime 差异进入变更集。
                }
                (Some(_), Some(_)) => changes.push(FileChange {
                    kind: FileChangeKind::Modified,
                    rel_path: rel.clone(),
                    size: *ws_map.get(rel).map(|(s, _)| s).unwrap_or(&0),
                }),
                (None, Some(_)) => changes.push(FileChange {
                    kind: FileChangeKind::Added,
                    rel_path: rel.clone(),
                    size: ws_map[rel].0,
                }),
                (Some((size, _)), None) => changes.push(FileChange {
                    kind: FileChangeKind::Deleted,
                    rel_path: rel.clone(),
                    size: *size,
                }),
                (None, None) => unreachable!("路径集合来自两边并集"),
            }
        }
        // 符号链接目标变化视为 Modified。
        let mut link_paths: HashSet<&String> = HashSet::new();
        link_paths.extend(snap_links.keys());
        link_paths.extend(ws_link_map.keys());
        for rel in link_paths {
            if snap_links.get(rel) != ws_link_map.get(rel) && !ws_map.contains_key(rel) {
                changes.push(FileChange {
                    kind: FileChangeKind::Modified,
                    rel_path: rel.clone(),
                    size: 0,
                });
            }
        }
        changes.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(changes)
    }

    /// 回滚工作区到指定快照。回滚前自动拍摄保护快照，保证回滚可撤销。
    pub fn restore_snapshot(
        &mut self,
        session_id: &str,
        snapshot_id: &str,
        workspace: &Path,
    ) -> Result<RestoreReport> {
        ensure_safe_id(session_id, "会话 ID")?;
        ensure_safe_id(snapshot_id, "快照 ID")?;
        if !workspace.is_dir() {
            bail!("工作区目录不存在：{}", workspace.display());
        }
        let meta = self.load_meta(session_id, snapshot_id)?;
        let protected = self
            .take_snapshot(
                session_id,
                workspace,
                usize::MAX,
                SnapshotReason::PreRestore,
            )
            .context("拍摄回滚前保护快照失败")?;

        let tree_dir = self.tree_dir(session_id, snapshot_id);
        let mut report = RestoreReport {
            protected_snapshot_id: Some(protected.id),
            ..Default::default()
        };

        // 恢复普通文件：先删再拷，避免 CoW 目标已存在与原地写残留。
        for entry in &meta.files {
            let src = tree_dir.join(&entry.rel_path);
            let dst = workspace.join(&entry.rel_path);
            if !src.is_file() {
                tracing::warn!(session_id, rel = %entry.rel_path, "快照条目缺失，跳过恢复");
                continue;
            }
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
            }
            if dst.exists() {
                fs::remove_file(&dst).with_context(|| dst.display().to_string())?;
            }
            copy::copy_file(&src, &dst).with_context(|| format!("恢复 {} 失败", entry.rel_path))?;
            report.restored_files += 1;
        }

        // 重建符号链接。
        for link in &meta.symlinks {
            let dst = workspace.join(&link.rel_path);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
            }
            if let Ok(meta) = fs::symlink_metadata(&dst)
                && meta.is_symlink()
            {
                let _ = fs::remove_file(&dst);
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&link.target, &dst)
                    .with_context(|| format!("重建符号链接 {} 失败", link.rel_path))?;
                report.restored_symlinks += 1;
            }
        }

        // 工作区中存在但快照中没有的条目移入暂存区（回收站语义，不直接删除）。
        let snap_paths: HashSet<&String> = meta
            .files
            .iter()
            .map(|f| &f.rel_path)
            .chain(meta.symlinks.iter().map(|l| &l.rel_path))
            .collect();
        let (ws_files, ws_links) = self.scan_workspace(workspace)?;
        let orphan_dir = self.orphan_dir(session_id, snapshot_id);
        for entry in ws_files
            .into_iter()
            .chain(ws_links.into_iter().map(|l| FileEntry {
                rel_path: l.rel_path,
                size: 0,
                mtime_ns: 0,
            }))
        {
            if snap_paths.contains(&entry.rel_path) {
                continue;
            }
            let src = workspace.join(&entry.rel_path);
            let dst = orphan_dir.join(&entry.rel_path);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
            }
            move_to_orphan(&src, &dst)?;
            report.orphaned_files += 1;
        }
        Ok(report)
    }

    /// 恢复单个文件到工作区。
    pub fn restore_file(
        &self,
        session_id: &str,
        snapshot_id: &str,
        rel_path: &str,
        workspace: &Path,
    ) -> Result<()> {
        ensure_safe_id(session_id, "会话 ID")?;
        ensure_safe_id(snapshot_id, "快照 ID")?;
        ensure_safe_rel_path(rel_path)?;
        let src = self.tree_dir(session_id, snapshot_id).join(rel_path);
        if !src.is_file() {
            bail!("快照中不存在该文件：{rel_path}");
        }
        let dst = workspace.join(rel_path);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
        }
        if dst.exists() {
            fs::remove_file(&dst).with_context(|| dst.display().to_string())?;
        }
        copy::copy_file(&src, &dst).with_context(|| rel_path.to_string())?;
        Ok(())
    }

    /// 恢复操作所用的已登记工作区（最近一次快照时的工作区路径）。
    /// 内存未命中（如宿主重启后）回落到 index.json 的持久化记录。
    pub fn known_workspace(&self, session_id: &str) -> Option<PathBuf> {
        if let Some(workspace) = self.workspaces.get(session_id) {
            return Some(workspace.clone());
        }
        let persisted = self.read_index(session_id).workspace?;
        (!persisted.is_empty()).then(|| PathBuf::from(persisted))
    }

    fn scan_workspace(&self, workspace: &Path) -> Result<(Vec<FileEntry>, Vec<SymlinkEntry>)> {
        let mut files = Vec::new();
        let mut links = Vec::new();
        walk(
            workspace,
            workspace,
            &self.config.ignore_names,
            &mut files,
            &mut links,
        )?;
        Ok((files, links))
    }

    /// 保留策略：数量上限 + 容量上限，从最旧开始淘汰。
    fn enforce_retention(&self, session_id: &str, index: &mut SessionIndex) {
        while index.snapshots.len() > self.config.max_snapshots_per_session {
            self.drop_oldest(session_id, index);
        }
        let total: u64 = index.snapshots.iter().map(|s| s.total_size).sum();
        if total > self.config.max_total_bytes_per_session {
            tracing::warn!(session_id, total, "快照区超过容量上限，开始淘汰最旧快照");
        }
        let mut total = total;
        while total > self.config.max_total_bytes_per_session && index.snapshots.len() > 1 {
            total =
                total.saturating_sub(index.snapshots.first().map(|s| s.total_size).unwrap_or(0));
            self.drop_oldest(session_id, index);
        }
    }

    fn drop_oldest(&self, session_id: &str, index: &mut SessionIndex) {
        if let Some(oldest) = index.snapshots.first().cloned() {
            let dir = self.snapshot_dir(session_id, &oldest.id);
            if let Err(err) = fs::remove_dir_all(&dir) {
                tracing::warn!(session_id, id = %oldest.id, error = %err, "淘汰旧快照失败");
            }
            index.snapshots.remove(0);
        }
    }
}

/// 校验外部传入的相对路径：拒绝绝对路径、`..` 组件、反斜杠与空值，
/// 防止恢复目标逃出工作区（RFC 0017 安全边界）。
fn ensure_safe_rel_path(rel_path: &str) -> Result<()> {
    if rel_path.is_empty()
        || rel_path.starts_with('/')
        || rel_path.starts_with('\\')
        || rel_path.contains(":\\")
        || rel_path.contains('\\')
    {
        bail!("非法相对路径：{rel_path}");
    }
    let path = Path::new(rel_path);
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("相对路径不允许包含 ..：{rel_path}");
    }
    Ok(())
}

/// 校验外部传入的标识（会话/快照 ID）：仅允许字母数字与 `-_.`，
/// 防止路径拼接逃逸。
fn ensure_safe_id(id: &str, label: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        bail!("非法{label}：{id}");
    }
}

fn fingerprint_map(meta: Option<&SnapshotMeta>) -> HashMap<String, (u64, i64)> {
    meta.map(|m| {
        m.files
            .iter()
            .map(|f| (f.rel_path.clone(), (f.size, f.mtime_ns)))
            .collect()
    })
    .unwrap_or_default()
}

/// 递归扫描目录：忽略名单按名称精确匹配；符号链接不深入；其余类型（fifo 等）忽略。
fn walk(
    dir: &Path,
    base: &Path,
    ignore: &[String],
    files: &mut Vec<FileEntry>,
    links: &mut Vec<SymlinkEntry>,
) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| dir.display().to_string())?;
    for entry in entries {
        let entry = entry.with_context(|| dir.display().to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if ignore.iter().any(|item| item == &name) {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| path.display().to_string())?;
        let rel_path = path
            .strip_prefix(base)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .map_err(|_| anyhow!("路径无法相对化：{}", path.display()))?;
        if file_type.is_symlink() {
            let target = fs::read_link(&path)
                .with_context(|| path.display().to_string())?
                .to_string_lossy()
                .into_owned();
            links.push(SymlinkEntry { rel_path, target });
        } else if file_type.is_dir() {
            walk(&path, base, ignore, files, links)?;
        } else if file_type.is_file() {
            let metadata = fs::metadata(&path).with_context(|| path.display().to_string())?;
            let mtime_ns = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            files.push(FileEntry {
                rel_path,
                size: metadata.len(),
                mtime_ns,
            });
        }
    }
    Ok(())
}

/// 移入暂存区：同卷 rename，失败回退拷贝后删除。
fn move_to_orphan(src: &Path, dst: &Path) -> Result<()> {
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    copy::copy_file(src, dst).with_context(|| src.display().to_string())?;
    fs::remove_file(src).with_context(|| src.display().to_string())?;
    Ok(())
}

#[cfg(test)]
mod path_guard_tests {
    use super::*;

    #[test]
    fn rel_path_escape_is_rejected() {
        assert!(ensure_safe_rel_path("../meta.json").is_err());
        assert!(ensure_safe_rel_path("a/../../etc/passwd").is_err());
        assert!(ensure_safe_rel_path("/etc/passwd").is_err());
        assert!(ensure_safe_rel_path("C:\\Windows\\system32").is_err());
        assert!(ensure_safe_rel_path("a\\b").is_err());
        assert!(ensure_safe_rel_path("").is_err());
        assert!(ensure_safe_rel_path("src/main.rs").is_ok());
        assert!(ensure_safe_rel_path("dist/index.html").is_ok());
    }

    #[test]
    fn unsafe_ids_are_rejected() {
        assert!(ensure_safe_id("../evil", "会话 ID").is_err());
        assert!(ensure_safe_id("a/b", "快照 ID").is_err());
        assert!(ensure_safe_id("", "会话 ID").is_err());
        assert!(ensure_safe_id("s1", "会话 ID").is_ok());
        assert!(ensure_safe_id("0123456789abcdef0123456789abcdef", "快照 ID").is_ok());
    }
}
