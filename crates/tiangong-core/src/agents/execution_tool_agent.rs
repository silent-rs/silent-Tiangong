//! 工具执行辅助函数（命令拆分、耗时计算）。
//!
//! core 内置工具规格已全部迁出至进程内插件（fs / fetch / command / browser / terminal），
//! plugin_injection synthetic tool 归位到 core/plugin_injection.rs。
//! 本文件仅保留被 MCP 工具执行链路复用的辅助函数。

/// 拆分命令字符串为参数列表（支持引号、转义）。
pub(crate) fn split_command_parts(raw: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' if !in_single => {
                escaped = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped || in_single || in_double {
        return None;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() { None } else { Some(out) }
}

pub(crate) fn elapsed_ms_u64(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}
