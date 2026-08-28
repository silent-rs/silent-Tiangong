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
    // sandbox-exec 无法再次应用沙箱——明确报告平台能力不可用。
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
    let writable = if policy.mode == super::policy::SandboxMode::WorkspaceWrite {
        policy.writable_roots()
    } else {
        Vec::new()
    };
    // SBPL 注入防护：路径会拼进 profile 文本，控制字符或非 UTF-8 内容可能
    // 破坏规则结构。不安全路径编译为拒绝一切的 profile（fail-closed），
    // 绝不放行可能被注入的规则。
    let paths_safe = writable
        .iter()
        .chain(policy.read_only_roots().iter())
        .chain(policy.denied_read_roots().iter())
        .all(|path| sbpl_safe_path(path));
    if !paths_safe {
        return "(version 1)\n(deny default)\n".to_string();
    }

    let mut sbpl = String::new();
    sbpl.push_str("(version 1)\n");
    // 读全盘放行（工具链需要读系统目录、SDK、home 缓存）。
    sbpl.push_str("(allow file-read*)\n");
    // 默认禁写，再逐个放行；/dev/null 是大量脚本的基础依赖。
    sbpl.push_str("(deny file-write*)\n");
    sbpl.push_str("(allow file-write* (literal \"/dev/null\"))\n");
    for root in &writable {
        let _ = writeln!(sbpl, "(allow file-write* (subpath \"{}\"))", escape(root));
    }
    // 防篡改段：写在 allow 之后才能重新锁死（后规则覆盖先规则）。
    for path in policy.read_only_roots() {
        let escaped = escape(&path);
        let _ = writeln!(sbpl, "(deny file-write* (literal \"{escaped}\"))");
        let _ = writeln!(sbpl, "(deny file-write* (subpath \"{escaped}\"))");
    }
    // 敏感路径读取规则位于全局 file-read 放行之后，精确路径与子路径都拒绝。
    for path in policy.denied_read_roots() {
        let escaped = escape(&path);
        let _ = writeln!(sbpl, "(deny file-read* (literal \"{escaped}\"))");
        let _ = writeln!(sbpl, "(deny file-read* (subpath \"{escaped}\"))");
    }
    if !policy.allow_network {
        sbpl.push_str("(deny network*)\n");
    }
    sbpl
}

/// 路径可安全进入 SBPL 文本：必须是 UTF-8 且不含控制字符
/// （换行/NUL 等会破坏 profile 结构）。括号在字符串字面量内无特殊含义，
/// 不拒绝以免误伤 macOS 常见文件名。
fn sbpl_safe_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|text| !text.chars().any(char::is_control))
}

/// 构造 sandbox-exec 的 argv 前缀（不含被包装命令本身）。
pub fn wrap_argv(policy: &SandboxPolicy) -> Vec<String> {
    vec!["-p".to_string(), compile_profile(policy)]
}

fn escape(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn profile_denies_write_outside_roots_and_network() {
        let mut policy = SandboxPolicy::workspace_write("/tmp/ws");
        policy.protected_paths = vec!["/tmp/ws/protected".into()];
        policy.denied_read_paths = vec!["/tmp/home/.ssh".into()];
        let sbpl = compile_profile(&policy);
        assert!(sbpl.contains("(deny file-write*)"));
        assert!(sbpl.contains("(deny network*)"));
        assert!(sbpl.contains("(allow file-read*)"));
        // 防篡改段位于工作区放行之后。
        let ws_allow = sbpl
            .find("(allow file-write* (subpath \"/tmp/ws\"))")
            .unwrap();
        let protected_deny = sbpl
            .find("(deny file-write* (subpath \"/tmp/ws/protected\"))")
            .unwrap();
        assert!(protected_deny > ws_allow);
        assert!(sbpl.contains("(deny file-read* (subpath \"/tmp/home/.ssh\"))"));
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
    fn wrap_argv_contains_only_launcher_arguments() {
        let policy = SandboxPolicy::workspace_write("/tmp/ws");
        let argv = wrap_argv(&policy);
        assert_eq!(argv[0], "-p");
        assert!(argv[1].contains("(version 1)"));
        assert!(!argv.iter().any(|arg| arg == SEATBELT_BIN));
    }

    #[test]
    fn control_character_path_compiles_to_deny_all() {
        // 换行注入尝试：不安全路径必须编译为拒绝一切的 profile，
        // 而不是让注入内容进入规则文本。
        let policy = SandboxPolicy::workspace_write("/tmp/ws\n(deny file-write*)");
        let sbpl = compile_profile(&policy);
        assert_eq!(sbpl, "(version 1)\n(deny default)\n");

        // 括号在字符串字面量内合法（macOS 常见文件名），不触发拒绝。
        let policy = SandboxPolicy::workspace_write("/tmp/demo(1)");
        let sbpl = compile_profile(&policy);
        assert!(sbpl.contains("(allow file-write* (subpath \"/tmp/demo(1)\"))"));
    }
}
