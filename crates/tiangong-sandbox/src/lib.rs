//! 沙箱执行层（RFC 0017）。
//!
//! S1 阶段交付 turn 边界快照与恢复服务：
//!
//! - [`engine::SnapshotEngine`]：扫描、增量快照（快照区内硬链接复用 + 平台写时复制）、
//!   变更集计算、回滚（回滚前自动拍摄保护快照，多出的文件移入暂存区而非直接删除）、
//!   数量与容量双重保留策略。
//! - [`service::SnapshotService`]：单工作线程串行处理，turn 钩子非阻塞入队，
//!   查询类调用经回执通道等待。
//! - [`plugin`]:以 core 插件形式在 `on_turn_finished` 触发快照，core 零改动。
//!
//! 后续阶段（S2+）在本 crate 内追加沙箱策略编译与 runner。

pub mod copy;
pub mod engine;
pub mod formats;
pub mod plugin;
pub mod service;

pub use engine::{SnapshotConfig, SnapshotEngine};
pub use formats::{FileChange, FileChangeKind, RestoreReport, SnapshotReason, SnapshotSummary};
pub use plugin::build_plugin;
pub use service::SnapshotService;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use super::*;

    /// 等待异步快照请求完成（服务为串行队列，用一次同步调用当屏障）。
    fn settle(service: &SnapshotService, session_id: &str) {
        let _ = service.list_snapshots(session_id);
        // list 与 Snapshot 同队列串行，返回即代表此前请求已处理。
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn snapshot_diff_and_restore_roundtrip() {
        let storage = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ws = workspace.path();
        let service = SnapshotService::new(storage.path(), SnapshotConfig::default());

        // turn 1：初始文件。
        write(&ws.join("a.txt"), "v1");
        write(&ws.join("src/b.txt"), "b");
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.txt", ws.join("link")).unwrap();
        service.request_snapshot("s1", ws, 0, SnapshotReason::Turn);
        settle(&service, "s1");

        // turn 2：修改 a、删除 b、新增 c。
        write(&ws.join("a.txt"), "v2-rewritten");
        fs::remove_file(ws.join("src/b.txt")).unwrap();
        write(&ws.join("c.txt"), "new file");
        service.request_snapshot("s1", ws, 1, SnapshotReason::Turn);
        settle(&service, "s1");

        let list = service.list_snapshots("s1").unwrap();
        assert_eq!(list.len(), 2);
        let first = &list[0].id;

        // 变更集：快照 1 之后工作区发生的变化。
        let changes = service.changeset("s1", first).unwrap();
        let mut kinds: Vec<(FileChangeKind, String)> = changes
            .iter()
            .map(|c| (c.kind, c.rel_path.clone()))
            .collect();
        kinds.sort_by(|a, b| a.1.cmp(&b.1));
        assert!(kinds.contains(&(FileChangeKind::Modified, "a.txt".into())));
        assert!(kinds.contains(&(FileChangeKind::Deleted, "src/b.txt".into())));
        assert!(kinds.contains(&(FileChangeKind::Added, "c.txt".into())));

        // 回滚到快照 1。
        let report = service.restore("s1", first).unwrap();
        assert_eq!(report.restored_files, 2); // a.txt 与 src/b.txt
        assert_eq!(report.orphaned_files, 1); // c.txt 移入暂存
        assert!(report.protected_snapshot_id.is_some());
        assert_eq!(fs::read_to_string(ws.join("a.txt")).unwrap(), "v1");
        assert_eq!(fs::read_to_string(ws.join("src/b.txt")).unwrap(), "b");
        assert!(!ws.join("c.txt").exists());
        #[cfg(unix)]
        assert_eq!(
            fs::read_link(ws.join("link")).unwrap().to_string_lossy(),
            "a.txt"
        );

        // 回滚本身可撤销：用保护快照把工作区恢复到回滚前状态。
        let protected = report.protected_snapshot_id.unwrap();
        let report2 = service.restore("s1", &protected).unwrap();
        assert!(report2.restored_files >= 1);
        assert_eq!(
            fs::read_to_string(ws.join("a.txt")).unwrap(),
            "v2-rewritten"
        );
    }

    #[test]
    fn incremental_snapshot_reuses_unchanged_files() {
        let storage = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ws = workspace.path();
        let service = SnapshotService::new(storage.path(), SnapshotConfig::default());

        write(&ws.join("stable.txt"), "never change");
        write(&ws.join("edit.txt"), "old");
        service.request_snapshot("s2", ws, 0, SnapshotReason::Turn);
        settle(&service, "s2");

        write(&ws.join("edit.txt"), "new contents");
        service.request_snapshot("s2", ws, 1, SnapshotReason::Turn);
        settle(&service, "s2");

        // 直接读引擎产物断言增量统计：第二个快照应复用 1 个、拷贝 1 个。
        let engine = SnapshotEngine::new(storage.path(), SnapshotConfig::default());
        let list = engine.list_snapshots("s2");
        assert_eq!(list.len(), 2);
        let second = engine.latest_snapshot("s2").unwrap();
        assert_eq!(second.reused, 1);
        assert_eq!(second.copied, 1);
        assert_eq!(second.file_count, 2);
    }

    #[test]
    fn restore_single_file() {
        let storage = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ws = workspace.path();
        let service = SnapshotService::new(storage.path(), SnapshotConfig::default());

        write(&ws.join("a.txt"), "origin");
        service.request_snapshot("s3", ws, 0, SnapshotReason::Turn);
        settle(&service, "s3");

        write(&ws.join("a.txt"), "broken");
        let first = service.list_snapshots("s3").unwrap()[0].id.clone();
        service.restore_file("s3", &first, "a.txt").unwrap();
        assert_eq!(fs::read_to_string(ws.join("a.txt")).unwrap(), "origin");
    }

    #[test]
    fn ignore_list_and_retention() {
        let storage = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ws = workspace.path();
        let config = SnapshotConfig {
            max_snapshots_per_session: 2,
            ..SnapshotConfig::default()
        };
        let service = SnapshotService::new(storage.path(), config);

        write(&ws.join("node_modules/pkg/index.js"), "ignored");
        write(&ws.join("keep.txt"), "kept");
        for turn in 0..3 {
            service.request_snapshot("s4", ws, turn, SnapshotReason::Turn);
        }
        settle(&service, "s4");

        let engine = SnapshotEngine::new(storage.path(), SnapshotConfig::default());
        let list = engine.list_snapshots("s4");
        assert_eq!(list.len(), 2, "保留策略应只留最近 2 个快照");
        // 被忽略目录不进快照。
        let latest = engine.latest_snapshot("s4").unwrap();
        assert_eq!(latest.file_count, 1);
        assert!(latest.files.iter().all(|f| f.rel_path == "keep.txt"));
        // 旧快照目录已删除。
        let index_json = fs::read_to_string(storage.path().join("s4/index.json")).unwrap();
        assert!(!index_json.is_empty());
    }

    #[test]
    fn service_reply_for_unknown_session_is_empty() {
        let storage = tempfile::tempdir().unwrap();
        let service = SnapshotService::new(storage.path(), SnapshotConfig::default());
        let list = service.list_snapshots("no-such-session").unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn concurrent_requests_are_serialized() {
        let storage = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ws = workspace.path();
        let service = SnapshotService::new(storage.path(), SnapshotConfig::default());

        write(&ws.join("a.txt"), "x");
        // 连续入队多个快照请求（模拟多轮 turn 快速结束）。
        for turn in 0..5 {
            service.request_snapshot("s5", ws, turn, SnapshotReason::Turn);
        }
        let _ = service.list_snapshots("s5"); // 屏障：队列清空。
        std::thread::sleep(Duration::from_millis(100));
        let list = service.list_snapshots("s5").unwrap();
        assert_eq!(list.len(), 5);
    }
}
