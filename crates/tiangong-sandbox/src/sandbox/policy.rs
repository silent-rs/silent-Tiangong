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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxPolicy {
    pub mode: SandboxMode,
    /// 会话工作区（WorkspaceWrite 模式下的主可写根）。
    pub workspace: PathBuf,
    /// 额外可写根（插件数据目录等）。
    pub extra_writable: Vec<PathBuf>,
    /// 即使位于可写根内也保持只读的宿主敏感路径。
    #[serde(default)]
    pub protected_paths: Vec<PathBuf>,
    /// 沙箱内不可读取的宿主敏感路径（用户凭据等）。
    #[serde(default)]
    pub denied_read_paths: Vec<PathBuf>,
    /// 是否放行出网（默认 false；放行走宿主代理体系，见 RFC D16）。
    pub allow_network: bool,
    /// 单次 command 的资源上限。
    #[serde(default)]
    pub resource_limits: SandboxResourceLimits,
}

/// 单次 command 进程树的资源上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxResourceLimits {
    /// 整次命令可消耗的 CPU 时间上限（秒）。
    pub max_cpu_time_seconds: u64,
    /// Unix 为单进程地址空间上限，Windows 为整个 Job 的提交内存上限。
    pub max_memory_bytes: u64,
    /// Linux 与 Windows 的进程数量上限。
    pub max_processes: u32,
}

impl Default for SandboxResourceLimits {
    fn default() -> Self {
        Self {
            max_cpu_time_seconds: 300,
            max_memory_bytes: 2 * 1024 * 1024 * 1024,
            max_processes: 64,
        }
    }
}

impl SandboxPolicy {
    /// 命令执行位的默认策略：workspace-write、禁网。
    pub fn workspace_write(workspace: impl Into<PathBuf>) -> Self {
        Self {
            mode: SandboxMode::WorkspaceWrite,
            workspace: workspace.into(),
            extra_writable: Vec::new(),
            protected_paths: Vec::new(),
            denied_read_paths: Vec::new(),
            allow_network: false,
            resource_limits: SandboxResourceLimits::default(),
        }
    }

    pub fn full_access() -> Self {
        Self {
            mode: SandboxMode::FullAccess,
            workspace: PathBuf::new(),
            extra_writable: Vec::new(),
            protected_paths: Vec::new(),
            denied_read_paths: Vec::new(),
            allow_network: true,
            resource_limits: SandboxResourceLimits::default(),
        }
    }

    /// 加入宿主权威的默认敏感读取拒绝项。
    ///
    /// 覆盖常见凭据位置：SSH/AWS/GPG/Kubernetes/Docker/Azure/GCP/GitHub CLI
    /// 与 `.netrc`。天工数据根下的配置、密钥与信任件同样拒绝读取。
    pub fn protect_user_credentials(&mut self, storage_root: &Path) {
        if let Some(home) = user_home_dir() {
            for path in home_credential_relative_paths() {
                self.denied_read_paths.push(home.join(path));
            }
        }
        for path in protected_storage_relative_paths() {
            self.denied_read_paths.push(storage_root.join(path));
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

    /// 防篡改段：宿主敏感路径保持只读。
    ///
    /// 当敏感根是工作区的祖先时不能整体锁死，否则默认的
    /// `<storage>/workspaces/<session>` 会失去写权限。白名单外本就默认禁写，
    /// 因此此时跳过祖先根仍会保护其它会话和宿主文件。
    /// 工作区内 `.git` 不在此列（用户裁定 2026-08-30）：agent 需要完整
    /// git 工作流，`.git` 与源码同等对待——可写。
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
        paths.sort();
        paths.dedup();
        paths
    }

    /// 不可读取路径（规范化、去重，只保留绝对路径）。
    pub fn denied_read_roots(&self) -> Vec<PathBuf> {
        let mut paths = self
            .denied_read_paths
            .iter()
            .filter(|path| path.is_absolute())
            .map(|path| canonical_or_keep(path))
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }
}

/// 当前用户家目录（绝对路径，解析失败返回 None）。
pub fn user_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let value = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            Some(PathBuf::from(drive).join(path))
        });
    #[cfg(not(windows))]
    let value = std::env::var_os("HOME").map(PathBuf::from);
    value.filter(|path| path.is_absolute())
}

/// 家目录下必须保持只读的凭据相对路径清单：沙箱禁读（deny-read）与
/// 家目录作工作区时的写保护（protected）共用同一份权威清单。
pub fn home_credential_relative_paths() -> [&'static str; 9] {
    [
        ".ssh",
        ".aws",
        ".gnupg",
        ".kube",
        ".docker",
        ".azure",
        ".config/gcloud",
        ".config/gh",
        ".netrc",
    ]
}

/// 家目录下凭据路径的绝对形态（家目录不可解析时为空）。
pub fn home_credential_paths() -> Vec<PathBuf> {
    let Some(home) = user_home_dir() else {
        return Vec::new();
    };
    home_credential_relative_paths()
        .iter()
        .map(|path| home.join(path))
        .collect()
}

/// storage_root 下对 sidecar 读写双禁的配置与信任件（相对路径段）：
/// 模型/服务/MCP/应用配置（app.json 含沙箱开关本身）、签名密钥与信任
/// 库、以及 Launcher 目录（沙箱防"被约束者"的核心——沙箱内进程必须
/// 够不到 Launcher 与信任锚，防替换逃逸）。其余存储目录整体开放可写。
pub fn protected_storage_relative_paths() -> [&'static str; 7] {
    [
        "keys",
        "trust.db",
        "mcp.json",
        "models.json",
        "server.json",
        "app.json",
        "sandbox",
    ]
}

/// protected_paths 用的绝对清单：存储配置件 + 家目录凭据（读写双禁）。
pub fn protected_paths_for(storage_root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = protected_storage_relative_paths()
        .iter()
        .map(|path| storage_root.join(path))
        .collect();
    paths.extend(home_credential_paths());
    paths
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
        let root = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::workspace_write(root.path().join("workspace"));
        let roots = policy.writable_roots();
        // 不自动包含全局系统临时目录：可写根 = 工作区 + 显式 extra。
        assert_eq!(roots.len(), 1);
        let mut policy = policy;
        policy.extra_writable = vec![root.path().join("exec-tmp")];
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

    #[test]
    fn policy_serialization_is_platform_stable() {
        let mut policy = SandboxPolicy::workspace_write("/workspace");
        policy.extra_writable = vec!["/execution/tmp".into()];
        policy.protected_paths = vec!["/workspace/.git".into()];
        policy.denied_read_paths = vec!["/home/user/.ssh".into()];

        let value = serde_json::to_value(&policy).unwrap();
        assert_eq!(value["mode"], "workspace_write");
        assert_eq!(value["workspace"], "/workspace");
        assert_eq!(value["allow_network"], false);
        assert_eq!(value["resource_limits"]["max_cpu_time_seconds"], 300);
        assert_eq!(value["resource_limits"]["max_processes"], 64);

        let decoded: SandboxPolicy = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, policy);
    }

    #[test]
    fn git_metadata_is_writable_in_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let git_file = workspace.path().join(".git");
        std::fs::write(&git_file, "gitdir: ../metadata").unwrap();
        let policy = SandboxPolicy::workspace_write(workspace.path());

        // .git 与源码同等对待（用户裁定 2026-08-30）：agent 需要完整 git
        // 工作流，不得出现在只读根中。
        assert!(
            !policy
                .read_only_roots()
                .contains(&canonical_or_keep(&git_file))
        );
    }

    #[test]
    fn credential_protection_covers_common_secret_locations() {
        let root = tempfile::tempdir().unwrap();
        let mut policy = SandboxPolicy::workspace_write("/workspace");
        policy.protect_user_credentials(root.path());
        let denied = policy.denied_read_roots();

        // 用户目录凭据位置。
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            for path in [".ssh", ".aws", ".gnupg", ".kube", ".docker", ".netrc"] {
                assert!(
                    denied.contains(&canonical_or_keep(&home.join(path))),
                    "应拒绝读取 {}",
                    path
                );
            }
        }
        // 天工数据根内的密钥与凭据文件。
        for path in ["keys", "trust.db", "mcp.json", "models.json"] {
            assert!(
                denied.contains(&canonical_or_keep(&root.path().join(path))),
                "应拒绝读取数据根内 {path}"
            );
        }
    }
}
