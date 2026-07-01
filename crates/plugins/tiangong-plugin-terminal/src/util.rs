//! 终端插件共享工具函数

/// 对单个 shell 参数进行安全引用。
///
/// 行为按目标平台区分（与 `common.rs::derive_shell_exec_args` 的跨平台 shell 选择一致）：
///
/// - **非 Windows**（bash/sh 等 POSIX shell）：若字符串为空或包含任何 shell 元字符
///   （空白、引号、`$`、`` ` ``、`!`、通配符、控制操作符等），则用单引号包裹并对
///   内部单引号做 `'\\''` 转义；否则原样返回。
/// - **Windows**（PowerShell/cmd）：Windows 路径天然含反斜杠 `\`，POSIX 单引号转义
///   在 PowerShell/cmd 下不工作。这里仅对含空格或双引号的参数用双引号包裹并对
///   内部双引号做 `""` 转义（cmd 兼容写法，PowerShell 同样接受）。反斜杠、`$` 等
///   不视为需要引用的元字符，避免把合法的 `C:\Users\...` 路径整体包进引号导致
///   `cd` 失败。
///
/// 该函数同时服务于：
/// - `handler.rs` 的 `format_command`（普通命令参数拼装）
/// - `manager.rs` 的 `SetCwd`（`cd <path>` 路径引用）
pub(crate) fn shell_quote(s: &str) -> String {
    if cfg!(target_os = "windows") {
        windows_quote(s)
    } else {
        posix_quote(s)
    }
}

/// POSIX shell（bash/sh）参数引用：单引号包裹，内部单引号转义为 `'\''`。
fn posix_quote(s: &str) -> String {
    if s.is_empty()
        || s.contains(|c: char| {
            c.is_whitespace()
                || c == '\''
                || c == '"'
                || c == '\\'
                || c == '$'
                || c == '`'
                || c == '!'
                || c == '*'
                || c == '?'
                || c == '['
                || c == ']'
                || c == '('
                || c == ')'
                || c == '{'
                || c == '}'
                || c == '|'
                || c == '&'
                || c == ';'
                || c == '<'
                || c == '>'
                || c == '~'
        })
    {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

/// Windows shell（PowerShell/cmd）参数引用：仅当含空格或双引号时用双引号包裹。
///
/// 不引用反斜杠：Windows 路径（`C:\Users\foo`）大量使用反斜杠，若按 POSIX 规则
/// 把含反斜杠的参数整体包进单引号，PowerShell 会原样保留单引号导致 `cd` 找不到路径。
/// 双引号在 cmd 下为转义标准（内部 `"` → `""`），PowerShell 同样接受。
fn windows_quote(s: &str) -> String {
    if s.is_empty() || s.contains(' ') || s.contains('"') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_simple_path_not_quoted() {
        assert_eq!(posix_quote("/usr/bin"), "/usr/bin");
    }

    #[test]
    fn posix_path_with_space_single_quoted() {
        assert_eq!(posix_quote("/path with space"), "'/path with space'");
    }

    #[test]
    fn posix_single_quote_escaped() {
        assert_eq!(posix_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn windows_drive_path_not_quoted() {
        // 关键回归点：Windows 路径含反斜杠不应被引用，否则 cd 会失败
        assert_eq!(windows_quote(r"C:\Users\foo"), r"C:\Users\foo");
    }

    #[test]
    fn windows_path_with_space_double_quoted() {
        assert_eq!(
            windows_quote(r"C:\Program Files\foo"),
            r#""C:\Program Files\foo""#
        );
    }

    #[test]
    fn windows_inner_double_quote_escaped() {
        assert_eq!(windows_quote(r#"a"b"#), r#""a""b""#);
    }

    #[test]
    fn windows_empty_quoted() {
        assert_eq!(windows_quote(""), "\"\"");
    }
}
