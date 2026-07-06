//! 子进程环境变量加载工具。
//!
//! `collect_runtime_env()` 的 skills 扫描逻辑已迁入 skill plugin 的 `collect_exec_env`
//! 实现；MCP server 的 env 由 mcp plugin 的 `collect_exec_env` 贡献。core 在所有插件
//! 注册完成后统一汇总各插件的 `collect_exec_env` 写入 RuntimeEngine。
//!
//! 本模块仅保留 `.env` / `.env.local` 文件解析工具，供 skill plugin 复用。

use std::path::Path;

/// 加载目录下的 .env.local / .env 文件。
pub fn load_local_env(cwd: &Path) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for file in [".env.local", ".env"] {
        let path = cwd.join(file);
        if !path.is_file() {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if !is_valid_env_key(key) {
                continue;
            }
            let value = normalize_env_value(value.trim());
            env.push((key.to_string(), value));
        }
    }
    env
}

fn is_valid_env_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    for (idx, ch) in key.chars().enumerate() {
        if idx == 0 && !(ch.is_ascii_alphabetic() || ch == '_') {
            return false;
        }
        if !(ch.is_ascii_alphanumeric() || ch == '_') {
            return false;
        }
    }
    true
}

fn normalize_env_value(raw: &str) -> String {
    let mut value = raw.trim().to_string();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0] as char;
        let last = bytes[value.len() - 1] as char;
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            value = value[1..value.len() - 1].to_string();
        }
    }
    value
}
