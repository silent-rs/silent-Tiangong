use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::Notify;

/// 配置文件变更监听器
///
/// 通过轮询文件修改时间检测变更，避免引入重依赖。
/// 检测到变更时通过 `Notify` 通知订阅者。
pub struct ConfigWatcher {
    /// 监听的文件路径及其上次修改时间
    files: HashMap<PathBuf, Option<SystemTime>>,
    /// 轮询间隔
    interval: Duration,
    /// 变更通知
    notify: Arc<Notify>,
}

impl ConfigWatcher {
    /// 创建配置监听器
    ///
    /// `interval`: 轮询间隔，建议 2-5 秒
    pub fn new(interval: Duration) -> Self {
        Self {
            files: HashMap::new(),
            interval,
            notify: Arc::new(Notify::new()),
        }
    }

    /// 添加监听文件
    pub fn watch(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        let mtime = file_mtime(&path);
        self.files.insert(path, mtime);
    }

    /// 获取变更通知 handle（可在其他 task 中 await）
    pub fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    /// 启动轮询（应在 tokio::spawn 中调用）
    ///
    /// 检测到任何文件变更时发出通知。永不返回。
    pub async fn run(&mut self) {
        loop {
            tokio::time::sleep(self.interval).await;

            let mut changed = false;
            for (path, last_mtime) in &mut self.files {
                let current_mtime = file_mtime(path);
                if current_mtime != *last_mtime {
                    tracing::info!(path = %path.display(), "检测到配置文件变更");
                    *last_mtime = current_mtime;
                    changed = true;
                }
            }

            if changed {
                self.notify.notify_waiters();
            }
        }
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
