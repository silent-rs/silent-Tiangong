//! 宿主权威工作区注册表（RFC 0017 透明执行封套）。
//!
//! 会话创建时由 core 登记权威工作区；沙箱策略的工作区校验以此为准——
//! 请求负载中的 `cwd` / `access.workspace` 只是候选，canonicalize 后
//! 必须位于本注册表内才可作为沙箱可写根（防"任意目录声明为工作区"提权）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn workspaces() -> &'static Mutex<HashSet<PathBuf>> {
    static WORKSPACES: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    WORKSPACES.get_or_init(|| Mutex::new(HashSet::new()))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 登记权威工作区（幂等；会话就绪时调用）。
pub fn register(path: &Path) {
    if path.as_os_str().is_empty() || !path.is_dir() {
        return;
    }
    if let Ok(mut set) = workspaces().lock() {
        set.insert(canonical(path));
    }
}

/// 判定候选路径是否为（或位于）某个权威工作区内。
pub fn is_authoritative(candidate: &Path) -> bool {
    let candidate = canonical(candidate);
    let Ok(set) = workspaces().lock() else {
        return false;
    };
    set.iter().any(|workspace| candidate.starts_with(workspace))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_workspace_and_children_are_authoritative() {
        let dir = tempfile::tempdir().unwrap();
        register(dir.path());
        assert!(is_authoritative(dir.path()));
        assert!(is_authoritative(&dir.path().join("sub/dir")));
    }

    #[test]
    fn unregistered_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        register(dir.path());
        assert!(!is_authoritative(outside.path()));
        assert!(!is_authoritative(Path::new("/etc")));
    }
}
