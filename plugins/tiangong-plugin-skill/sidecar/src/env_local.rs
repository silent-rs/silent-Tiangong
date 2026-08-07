//! `.env.local` / `.env` 解析（从 `tiangong_core::runtime_env` 复制，sidecar 自治）。

use std::path::Path;

/// 读取目录下的 `.env.local` 与 `.env`，返回解析出的键值对（.env.local 在前）。
///
/// 逐行解析：跳过空行与 `#` 注释，按首个 `=` 分割，key 校验为合法环境变量名，
/// value 剥掉首尾成对引号。
pub fn load_local_env(cwd: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in [".env.local", ".env"] {
        let path = cwd.join(name);
        if !path.is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if !is_valid_env_key(key) {
                continue;
            }
            let value = normalize_env_value(value.trim());
            out.push((key.to_string(), value));
        }
    }
    out
}

fn is_valid_env_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn normalize_env_value(raw: &str) -> String {
    let value = raw.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}
