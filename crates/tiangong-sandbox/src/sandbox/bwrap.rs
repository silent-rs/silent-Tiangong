//! Linux bubblewrap 编译器。
//!
//! 全盘只读挂载 + 可写根分层 bind；受保护路径以只读 bind 覆盖
//! （bwrap 按参数顺序挂载，后 bind 同一路径覆盖先 bind）。

use std::path::Path;

use super::policy::{SandboxMode, SandboxPolicy};

const CANDIDATE_BINS: [&str; 3] = ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"];

pub fn bwrap_available() -> Option<String> {
    if let Ok(path) = std::env::var("TIANGONG_SANDBOX_BWRAP")
        && Path::new(&path).exists()
    {
        return Some(path);
    }
    CANDIDATE_BINS
        .iter()
        .find(|bin| Path::new(bin).exists())
        .map(|bin| bin.to_string())
}

/// 编译为 bwrap 参数前缀（不含 `--` 与被包装命令）。
pub fn wrap_argv(policy: &SandboxPolicy, bwrap_bin: &str) -> Vec<String> {
    let mut argv = vec![bwrap_bin.to_string()];
    // 全盘只读镜像。
    argv.push("--ro-bind".into());
    argv.push("/".into());
    argv.push("/".into());
    argv.push("--dev".into());
    argv.push("/dev".into());
    argv.push("--proc".into());
    argv.push("/proc".into());
    // 可写根。
    if policy.mode == SandboxMode::WorkspaceWrite {
        for root in policy.writable_roots() {
            let root = root.display().to_string();
            argv.push("--bind".into());
            argv.push(root.clone());
            argv.push(root);
        }
    }
    // 防篡改段：只读 bind 覆盖（在可写 bind 之后声明才生效）。
    for path in policy.protected_paths() {
        if path.exists() {
            let path = path.display().to_string();
            argv.push("--ro-bind".into());
            argv.push(path.clone());
            argv.push(path);
        }
    }
    argv.push("--unshare-user".into());
    argv.push("--unshare-pid".into());
    if !policy.allow_network {
        argv.push("--unshare-net".into());
    }
    argv.push("--die-with-parent".into());
    // 环境清理责任在调用方（如 command sidecar 的 env_clear + 白名单重建）：
    // 此处若 --clearenv 会把宿主注入的环境变量一并清掉。
    argv.push("--".into());
    argv
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn argv_orders_ro_bind_before_writable_and_protection() {
        let policy = SandboxPolicy::workspace_write("/tmp/ws");
        let argv = wrap_argv(&policy, "/usr/bin/bwrap");
        let ro_root = argv.iter().position(|a| a == "--ro-bind").unwrap();
        let bind_ws = argv.iter().position(|a| a == "/tmp/ws").unwrap();
        assert!(bind_ws > ro_root);
        assert!(argv.contains(&"--unshare-net".to_string()));
        assert!(argv.last().unwrap() == "--");
    }
}
