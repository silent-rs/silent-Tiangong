//! 终端插件共享工具函数

/// 对单个 shell 参数进行安全引用。
///
/// 若字符串为空或包含任何 shell 元字符（空白、引号、`$`、`` ` ``、`!`、通配符、
/// 控制操作符等），则用单引号包裹并对内部单引号做 `'\\''` 转义；否则原样返回。
///
/// 该函数同时服务于：
/// - `handler.rs` 的 `format_command`（普通命令参数拼装）
/// - `manager.rs` 的 `SetCwd`（`cd <path>` 路径引用）
pub(crate) fn shell_quote(s: &str) -> String {
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
