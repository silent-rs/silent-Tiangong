//! 沙箱策略：平台无关的中间表示（IR）。
//!
//! command 工具的宿主调用上下文最终归一为 [`SandboxPolicy`]；插件载荷和
//! manifest 不参与安全策略决策。

use std::path::{Path, PathBuf};

/// 沙箱模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    /// 全盘只读，无任何可写路径。
    ReadOnly,
    /// 读全盘放行、写限白名单（工作区 + 临时目录 + 额外可写）、网络默认禁。
    WorkspaceWrite,
    /// 仅供宿主显式特殊路径表示无沙箱；当前 Launcher 始终拒绝该模式。
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
    /// 即使位于可写根内也保持只读的宿主敏感路径。
    #[serde(default)]
    pub protected_paths: Vec<PathBuf>,
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
            protected_paths: Vec::new(),
            allow_network: false,
        }
    }

    pub fn full_access() -> Self {
        Self {
            mode: SandboxMode::FullAccess,
            workspace: PathBuf::new(),
            extra_writable: Vec::new(),
            protected_paths: Vec::new(),
            allow_network: true,
        }
    }

    /// 全部可写根（已规范化去重）：工作区、临时目录、额外可写。
    pub fn writable_roots(&self) -> Vec<PathBuf> {
        if self.mode != SandboxMode::WorkspaceWrite {
            return Vec::new();
        }
        // 不自动放行全局系统临时目录（审查修订）：每次执行的专用临时目录
        // 由宿主创建并经 extra_writable 显式传入。
        let mut roots = vec![self.workspace.clone()];
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

    /// 防篡改段：宿主敏感路径与工作区内 `.git` 保持只读。
    ///
    /// 当敏感根是工作区的祖先时不能整体锁死，否则默认的
    /// `<storage>/workspaces/<session>` 会失去写权限。白名单外本就默认禁写，
    /// 因此此时跳过祖先根仍会保护其它会话和宿主文件。
    pub fn read_only_roots(&self) -> Vec<PathBuf> {
        let writable = self.writable_roots();
        let mut paths = self
            .protected_paths
            .iter()
            .map(|path| canonical_or_keep(path))
            .filter(|protected| {
                !writable
                    .iter()
                    .any(|root| root != protected && root.starts_with(protected))
            })
            .collect::<Vec<_>>();
        let git = canonical_or_keep(&self.workspace.join(".git"));
        if git.is_dir() {
            paths.push(git);
        }
        paths.sort();
        paths.dedup();
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
        let policy = SandboxPolicy::workspace_write("/definitely/not/exists");
        let roots = policy.writable_roots();
        // 不自动包含全局系统临时目录：可写根 = 工作区 + 显式 extra。
        assert_eq!(roots.len(), 1);
        let mut policy = policy;
        policy.extra_writable = vec![std::env::temp_dir().join("exec-tmp")];
        assert_eq!(policy.writable_roots().len(), 2);
    }

    #[test]
    fn full_access_has_no_writable_roots() {
        assert!(SandboxPolicy::full_access().writable_roots().is_empty());
    }

    #[test]
    fn protected_storage_root_does_not_shadow_nested_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspaces/session");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut policy = SandboxPolicy::workspace_write(&workspace);
        policy.protected_paths = vec![root.path().to_path_buf()];

        assert!(
            !policy
                .read_only_roots()
                .contains(&canonical_or_keep(root.path()))
        );
        assert_eq!(policy.writable_roots(), vec![canonical_or_keep(&workspace)]);
    }

    #[test]
    fn protected_storage_root_remains_locked_for_broad_workspace() {
        let root = tempfile::tempdir().unwrap();
        let mut policy = SandboxPolicy::workspace_write(root.path().parent().unwrap());
        policy.protected_paths = vec![root.path().to_path_buf()];

        assert!(
            policy
                .read_only_roots()
                .contains(&canonical_or_keep(root.path()))
        );
    }
}
