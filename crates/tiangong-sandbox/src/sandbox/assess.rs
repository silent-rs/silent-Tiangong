//! 命令预分类器（RFC 0017 D11）。
//!
//! 静态评估仅用于粗筛与告警，不构成安全边界——真正的边界是 OS 沙箱：
//! 已知危险命令直接拒绝并引导走升级审批，未知命令一律进沙箱执行。

/// 静态风险评级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRisk {
    /// 已知安全（只读、幂等的常用命令）。
    KnownSafe,
    /// 未知：进沙箱执行。
    Unknown,
    /// 已知危险：拒绝执行，引导走升级审批（全权通道）。
    KnownDangerous,
}

/// 只读或幂等的常用命令。
const SAFE_PROGRAMS: [&str; 22] = [
    "ls", "cat", "pwd", "echo", "whoami", "date", "wc", "head", "tail", "grep", "find", "which",
    "file", "stat", "du", "df", "ps", "env", "printenv", "git", "node", "python3",
];

/// 危险程序名单：无论参数如何一律拒绝（需走全权审批通道）。
const DANGEROUS_PROGRAMS: [&str; 9] = [
    "mkfs",
    "fdisk",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "systemctl",
    "sysctl",
    "diskutil",
];

/// 脚本中的危险模式（run_shell 的整段脚本按此扫描）。
const DANGEROUS_PATTERNS: [&str; 10] = [
    "rm -rf /",       // 根目录递归删除
    "rm -rf ~",       // home 递归删除
    "rm -rf /*",      // 根通配
    "mkfs",           // 格式化
    "dd if=",         // 磁盘镜像写入（含 of=/dev/*）
    ":(){ :|:& };:",  // fork 炸弹
    "chmod -R 777 /", // 根目录权限放开
    "curl http",      // 网络取数（沙箱内网络已禁；直接拦截给清晰提示）
    "wget http",      // 同上
    "> /dev/sd",      // 直写块设备
];

/// 单程序 + 参数评估（run_command 场景）。
pub fn assess_program(program: &str, args: &[String]) -> CommandRisk {
    let name = program
        .rsplit('/')
        .next()
        .unwrap_or(program)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    if DANGEROUS_PROGRAMS.iter().any(|danger| {
        name == *danger || name.starts_with(danger) && name[danger.len()..].starts_with('.')
    }) {
        return CommandRisk::KnownDangerous;
    }
    // dd 仅在 of= 指向块设备时危险；简化处理：dd 一律危险。
    if name == "dd" {
        return CommandRisk::KnownDangerous;
    }
    if SAFE_PROGRAMS.contains(&name.as_str()) {
        // 安全命令携带可疑输出重定向时降级为未知（如 ls > /etc/passwd）。
        let writes_outside = args
            .iter()
            .any(|arg| arg.starts_with('>') && !arg.starts_with(">> ") || arg == ">");
        return if writes_outside {
            CommandRisk::Unknown
        } else {
            CommandRisk::KnownSafe
        };
    }
    CommandRisk::Unknown
}

/// 整段脚本评估（run_shell 场景）。
pub fn assess_script(script: &str) -> CommandRisk {
    for pattern in DANGEROUS_PATTERNS {
        if script.contains(pattern) {
            return CommandRisk::KnownDangerous;
        }
    }
    CommandRisk::Unknown
}

/// 生成拒绝执行的提示文本（回给 Agent，引导走升级审批）。
pub fn denial_hint(command_desc: &str) -> String {
    format!(
        "命令被预分类器判定为高危，未执行：{command_desc}。\
如确需执行，请先调用 request_user（kind: approval）向用户说明影响并获得批准，\
再以已批准的方式申请全权执行。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_programs_pass() {
        assert_eq!(
            assess_program("ls", &["-la".into()]),
            CommandRisk::KnownSafe
        );
        assert_eq!(
            assess_program("/usr/bin/git", &["status".into()]),
            CommandRisk::KnownSafe
        );
    }

    #[test]
    fn dangerous_programs_rejected() {
        assert_eq!(
            assess_program("mkfs.ext4", &[]),
            CommandRisk::KnownDangerous
        );
        assert_eq!(assess_program("dd", &[]), CommandRisk::KnownDangerous);
        assert_eq!(
            assess_program("shutdown", &["-h".into(), "now".into()]),
            CommandRisk::KnownDangerous
        );
    }

    #[test]
    fn unknown_by_default() {
        assert_eq!(
            assess_program("cargo", &["build".into()]),
            CommandRisk::Unknown
        );
    }

    #[test]
    fn safe_with_redirection_is_unknown() {
        assert_eq!(
            assess_program("ls", &[">".into(), "/etc/hosts".into()]),
            CommandRisk::Unknown
        );
    }

    #[test]
    fn script_patterns_detected() {
        assert_eq!(
            assess_script("echo hi && rm -rf /"),
            CommandRisk::KnownDangerous
        );
        assert_eq!(assess_script("cargo build --release"), CommandRisk::Unknown);
    }

    #[test]
    fn hint_mentions_approval() {
        assert!(denial_hint("mkfs").contains("request_user"));
    }
}
