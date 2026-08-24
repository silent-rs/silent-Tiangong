//! 沙箱策略：平台无关的中间表示（IR）。
//!
//! 两个策略来源（RFC 0017 D10）最终都归一为 [`SandboxPolicy`]：
//! 命令执行位（会话工作区 + 升级状态）与 sidecar 位（manifest 声明）。

use std::path::{Path, PathBuf};

/// 沙箱模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    /// 全盘只读，无任何可写路径。
    ReadOnly,
    /// 读全盘放行、写限白名单（工作区 + 临时目录 + 额外可写）、网络默认禁。
    WorkspaceWrite,
    /// 不沙箱（升级审批后或全信模式）。
    FullAccess,
}

/// 平台无关的沙箱策略（可序列化，经 Launcher 协议传输）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SandboxPolicy {
    pub mode: SandboxMode,
    /// 会话工作区（WorkspaceWrite 模式下的主可写根）。
    pub workspace: PathBuf,
    /// 额外可写根（插件数据目录等）。
    pub extra_writable: Vec<PathBuf>,
    /// 是否放行出网（默认 false；放行走宿主代理体系，见 RFC D16）。
    pub allow_network: bool,
}

impl SandboxPolicy {
    /// 命令执行位的默认策略：workspace-write、禁网。
    pub fn workspace_write(workspace: impl Into<PathBuf>) -> Self {
        Self {
            mode: SandboxMode::WorkspaceWrite,
            workspace: workspace.into(),
            extra_writable: Vec::new(),
            allow_network: false,
        }
    }

    pub fn full_access() -> Self {
        Self {
            mode: SandboxMode::FullAccess,
            workspace: PathBuf::new(),
            extra_writable: Vec::new(),
            allow_network: true,
        }
    }

    /// 全部可写根（已规范化去重）：工作区、临时目录、额外可写。
    pub fn writable_roots(&self) -> Vec<PathBuf> {
        if self.mode != SandboxMode::WorkspaceWrite {
            return Vec::new();
        }
        let mut roots = vec![self.workspace.clone(), std::env::temp_dir()];
        roots.extend(self.extra_writable.iter().cloned());
        let mut seen: Vec<PathBuf> = Vec::new();
        roots
            .into_iter()
            .filter(|p| p.is_absolute())
            .map(|p| canonical_or_keep(&p))
            .filter(|p| {
                if seen.contains(p) {
                    false
                } else {
                    seen.push(p.clone());
                    true
                }
            })
            .collect()
    }

    /// 防篡改段：无论如何声明，这些路径对写操作只读（RFC D14）。
    /// - 宿主数据目录（信任库、设置、快照）
    /// - 工作区内 `.git`（能改工作区，不能改 git 历史）
    pub fn protected_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![tiangong_config::io::storage_root()];
        let git = canonical_or_keep(&self.workspace.join(".git"));
        if git.is_dir() {
            paths.push(git);
        }
        paths
    }
}

/// 规范化路径：能解析则用真实路径（macOS 上 `/var` → `/private/var`），
/// 解析失败（路径不存在）保留原样。
pub fn canonical_or_keep(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path))
}

/// 无需文件系统访问的路径规范化：拆段、处理 `.` 与 `..`、去尾部 `/`。
fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for comp in path.components() {
        use std::path::Component;
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    let mut out = PathBuf::new();
    for part in parts {
        out.push(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_roots_are_canonical_and_deduped() {
        let tmp = std::env::temp_dir();
        let policy = SandboxPolicy::workspace_write("/definitely/not/exists");
        let roots = policy.writable_roots();
        assert!(roots.contains(&tmp.canonicalize().unwrap()));
        assert_eq!(roots.len(), 2);
    }

    #[test]
    fn full_access_has_no_writable_roots() {
        assert!(SandboxPolicy::full_access().writable_roots().is_empty());
    }
}
