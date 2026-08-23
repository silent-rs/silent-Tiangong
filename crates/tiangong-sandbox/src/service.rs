//! 快照服务：单工作线程串行处理请求。
//!
//! `on_turn_finished` 钩子运行在无 tokio runtime 的短命线程上，只做非阻塞入队；
//! 拍摄、比对、回滚等耗时操作全部在服务自己的工作线程串行执行。
//! 查询类请求经回执通道阻塞等待结果（Tauri 命令层应放入 `spawn_blocking`）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::engine::{SnapshotConfig, SnapshotEngine};
use crate::formats::{FileChange, RestoreReport, SnapshotReason, SnapshotSummary};

enum Request {
    Snapshot {
        session_id: String,
        workspace: PathBuf,
        turn_start_idx: usize,
        reason: SnapshotReason,
    },
    List {
        session_id: String,
        reply: ReplySender<Vec<SnapshotSummary>>,
    },
    Changeset {
        session_id: String,
        snapshot_id: String,
        reply: ReplySender<Vec<FileChange>>,
    },
    Restore {
        session_id: String,
        snapshot_id: String,
        reply: ReplySender<RestoreReport>,
    },
    RestoreFile {
        session_id: String,
        snapshot_id: String,
        rel_path: String,
        reply: ReplySender<()>,
    },
}

type ReplySender<T> = mpsc::Sender<std::result::Result<T, String>>;

pub struct SnapshotService {
    tx: mpsc::Sender<Request>,
}

impl SnapshotService {
    /// 创建服务并启动工作线程。
    pub fn new(root: impl Into<PathBuf>, config: SnapshotConfig) -> Arc<Self> {
        let (tx, rx) = mpsc::channel::<Request>();
        let mut engine = SnapshotEngine::new(root, config);
        std::thread::Builder::new()
            .name("tiangong-snapshot".to_string())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    handle(&mut engine, request);
                }
            })
            .expect("启动快照工作线程失败");
        Arc::new(Self { tx })
    }

    /// 进程级单例：快照根目录为 `<storage_root>/snapshots`。
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<SnapshotService>> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Self::new(default_root(), SnapshotConfig::default()))
            .clone()
    }

    /// 非阻塞触发快照（turn 钩子使用；服务停止时静默丢弃）。
    pub fn request_snapshot(
        &self,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        turn_start_idx: usize,
        reason: SnapshotReason,
    ) {
        let _ = self.tx.send(Request::Snapshot {
            session_id: session_id.into(),
            workspace: workspace.into(),
            turn_start_idx,
            reason,
        });
    }

    /// 快照摘要列表。
    pub fn list_snapshots(&self, session_id: &str) -> Result<Vec<SnapshotSummary>> {
        self.call(|reply| Request::List {
            session_id: session_id.to_string(),
            reply,
        })
    }

    /// 工作区与指定快照的差异。
    pub fn changeset(&self, session_id: &str, snapshot_id: &str) -> Result<Vec<FileChange>> {
        self.call(|reply| Request::Changeset {
            session_id: session_id.to_string(),
            snapshot_id: snapshot_id.to_string(),
            reply,
        })
    }

    /// 回滚工作区到指定快照（使用最近一次快照时登记的工作区路径）。
    pub fn restore(&self, session_id: &str, snapshot_id: &str) -> Result<RestoreReport> {
        self.call(|reply| Request::Restore {
            session_id: session_id.to_string(),
            snapshot_id: snapshot_id.to_string(),
            reply,
        })
    }

    /// 恢复单个文件。
    pub fn restore_file(&self, session_id: &str, snapshot_id: &str, rel_path: &str) -> Result<()> {
        self.call(|reply| Request::RestoreFile {
            session_id: session_id.to_string(),
            snapshot_id: snapshot_id.to_string(),
            rel_path: rel_path.to_string(),
            reply,
        })
    }

    /// 发送带回执的请求并阻塞等待结果。快照任务耗时较长时查询会排队，
    /// 等待上限 10 分钟（单次快照的极端预算）。
    fn call<T, F>(&self, build: F) -> Result<T>
    where
        F: FnOnce(ReplySender<T>) -> Request,
    {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(build(reply_tx))
            .map_err(|_| anyhow!("快照服务已停止"))?;
        reply_rx
            .recv_timeout(Duration::from_secs(600))
            .map_err(|_| anyhow!("等待快照服务响应超时"))?
            .map_err(|msg| anyhow!(msg))
    }
}

fn handle(engine: &mut SnapshotEngine, request: Request) {
    match request {
        Request::Snapshot {
            session_id,
            workspace,
            turn_start_idx,
            reason,
        } => {
            if let Err(err) = engine.take_snapshot(&session_id, &workspace, turn_start_idx, reason)
            {
                tracing::warn!(session_id, error = %err, "拍摄快照失败");
            }
        }
        Request::List { session_id, reply } => {
            let result = engine.list_snapshots(&session_id);
            let _ = reply.send(Ok(result));
        }
        Request::Changeset {
            session_id,
            snapshot_id,
            reply,
        } => {
            let workspace = engine.known_workspace(&session_id).map(Path::to_path_buf);
            let result = workspace
                .ok_or_else(|| anyhow!("该会话尚未登记工作区（未拍摄过快照）").to_string())
                .and_then(|workspace| {
                    engine
                        .changeset_vs_workspace(&session_id, &snapshot_id, &workspace)
                        .map_err(|err| err.to_string())
                });
            let _ = reply.send(result);
        }
        Request::Restore {
            session_id,
            snapshot_id,
            reply,
        } => {
            let workspace = engine.known_workspace(&session_id).map(Path::to_path_buf);
            let result = workspace
                .ok_or_else(|| anyhow!("该会话尚未登记工作区（未拍摄过快照）").to_string())
                .and_then(|workspace| {
                    engine
                        .restore_snapshot(&session_id, &snapshot_id, &workspace)
                        .map_err(|err| err.to_string())
                });
            let _ = reply.send(result);
        }
        Request::RestoreFile {
            session_id,
            snapshot_id,
            rel_path,
            reply,
        } => {
            let workspace = engine.known_workspace(&session_id).map(Path::to_path_buf);
            let result = workspace
                .ok_or_else(|| anyhow!("该会话尚未登记工作区（未拍摄过快照）").to_string())
                .and_then(|workspace| {
                    engine
                        .restore_file(&session_id, &snapshot_id, &rel_path, &workspace)
                        .map_err(|err| err.to_string())
                });
            let _ = reply.send(result);
        }
    }
}

fn default_root() -> PathBuf {
    Path::new(&tiangong_config::io::storage_root()).join("snapshots")
}
