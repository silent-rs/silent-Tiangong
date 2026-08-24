//! macOS Seatbelt profile 编译器。
//!
//! 采用 allow-by-default 基础 + 定点 deny 的 SBPL：未提及的类别默认放行，
//! 显式拒绝写操作与网络，再以更靠后的 subpath 规则放行可写白名单并
//! 重新锁死防篡改路径（后规则覆盖先规则）。这是 Codex 验证过的规则顺序。

use std::fmt::Write as _;
use std::path::Path;

use super::policy::SandboxPolicy;

const SEATBELT_BIN: &str = "/usr/bin/sandbox-exec";

pub fn seatbelt_available() -> bool {
    if !Path::new(SEATBELT_BIN).exists() {
        return false;
    }
    // 一次性真实探测：宿主环境若已在 Seatbelt 沙箱内（嵌套终端/受限 CI 外壳），
    // sandbox-exec 无法再次应用沙箱——按平台不可用降级（快照层兜底）。
    static PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PROBE.get_or_init(|| {
        std::process::Command::new(SEATBELT_BIN)
            .arg("-p")
            .arg("(version 1)")
            .arg("/usr/bin/true")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// 编译为 SBPL profile 文本。
pub fn compile_profile(policy: &SandboxPolicy) -> String {
    let mut sbpl = String::new();
    sbpl.push_str("(version 1)\n");
    // 读全盘放行（工具链需要读系统目录、SDK、home 缓存）。
    sbpl.push_str("(allow file-read*)\n");
    // 默认禁写，再逐个放行；/dev/null 是大量脚本的基础依赖。
    sbpl.push_str("(deny file-write*)\n");
    sbpl.push_str("(allow file-write* (literal \"/dev/null\"))\n");
    let writable = if policy.mode == super::policy::SandboxMode::WorkspaceWrite {
        policy.writable_roots()
    } else {
        Vec::new()
    };
    for root in &writable {
        let _ = writeln!(sbpl, "(allow file-write* (subpath \"{}\"))", escape(root));
    }
    // 防篡改段：写在 allow 之后才能重新锁死（后规则覆盖先规则）。
    for path in policy.protected_paths() {
        let _ = writeln!(sbpl, "(deny file-write* (subpath \"{}\"))", escape(&path));
    }
    if !policy.allow_network {
        sbpl.push_str("(deny network*)\n");
    }
    sbpl
}

/// 构造 sandbox-exec 的 argv 前缀（不含被包装命令本身）。
pub fn wrap_argv(policy: &SandboxPolicy) -> Vec<String> {
    vec![
        SEATBELT_BIN.to_string(),
        "-p".to_string(),
        compile_profile(policy),
    ]
}

fn escape(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_denies_write_outside_roots_and_network() {
        let policy = SandboxPolicy::workspace_write("/tmp/ws");
        let sbpl = compile_profile(&policy);
        assert!(sbpl.contains("(deny file-write*)"));
        assert!(sbpl.contains("(deny network*)"));
        assert!(sbpl.contains("(allow file-read*)"));
        // 防篡改段位于工作区放行之后。
        let ws_allow = sbpl
            .find("(allow file-write* (subpath \"/tmp/ws\"))")
            .unwrap();
        let tiangong_deny = sbpl.find(".tiangong").unwrap();
        assert!(tiangong_deny > ws_allow);
    }

    #[test]
    fn readonly_mode_has_no_writable_roots() {
        let mut policy = SandboxPolicy::workspace_write("/tmp/ws");
        policy.mode = crate::sandbox::policy::SandboxMode::ReadOnly;
        let sbpl = compile_profile(&policy);
        assert!(!sbpl.contains("subpath \"/tmp/ws"));
        assert!(sbpl.contains("(deny file-write*)"));
    }

    #[test]
    fn wrap_argv_leads_with_sandbox_exec() {
        let policy = SandboxPolicy::workspace_write("/tmp/ws");
        let argv = wrap_argv(&policy);
        assert_eq!(argv[0], "/usr/bin/sandbox-exec");
        assert_eq!(argv[1], "-p");
        assert!(argv[2].contains("(version 1)"));
    }
}
