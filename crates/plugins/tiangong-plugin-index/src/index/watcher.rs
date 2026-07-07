use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc;

#[allow(dead_code)]
pub enum FileEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}

#[allow(dead_code)]
pub struct FileWatcher {
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<FileEvent>,
}

#[allow(dead_code)]
impl FileWatcher {
    pub fn start(root: &std::path::Path) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<FileEvent>();
        let root = root.to_path_buf();
        let watch_root = root.clone();

        let tx_clone = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let Ok(event) = res else { return };
            let Some(path) = event.paths.first().cloned() else {
                return;
            };
            if path.starts_with(&root) {
                let _ = tx_clone.send(match event.kind {
                    EventKind::Create(_) => FileEvent::Created(path),
                    EventKind::Modify(_) => FileEvent::Modified(path),
                    EventKind::Remove(_) => FileEvent::Deleted(path),
                    _ => return,
                });
            }
        })?;

        watcher.watch(&watch_root, RecursiveMode::Recursive)?;

        Ok(Self {
            _watcher: watcher,
            rx,
        })
    }

    pub fn try_recv(&self) -> Option<FileEvent> {
        self.rx.try_recv().ok()
    }
}
