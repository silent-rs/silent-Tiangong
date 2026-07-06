//! Skills 环境变量收集（供子进程执行注入）。
//!
//! 原属 `tool/run_command.rs::collect_runtime_env`，LocalToolExecutor 删除后迁出至此，
//! 供 RuntimeEngine 在构造时收集、command 插件在 register 时读取。
//!
//! Skills 的环境变量直接从磁盘扫描（`~/.tiangong/skills/<id>/`），
//! 不再经过 `AgentConfig.skills`——skills 已从 AgentConfig 脱离，由 skill plugin 自治。
//! 这里扫描 enabled（skill.toml.available=true）的 skill 目录，读其 `.env.local`。
//!
//! MCP server 的环境变量已随 MCP 管理插件化迁出（由 mcp plugin 自管），core 不再收集。
//! MCP server 子进程自身的 env 在其 spawn 时由 mcp client 直接注入。

use std::collections::BTreeMap;
use std::path::Path;

use crate::app_state::default_skills_storage_dir_path;
use crate::skill::{read_skill_manifest, scan_skill_registry};

/// 从磁盘 skills 收集环境变量。
///
/// skill env 来自磁盘扫描 `~/.tiangong/skills/` 下 `available=true` 的 skill
/// 目录的 `.env.local`。MCP env 已迁出（由 mcp plugin 自管）。
pub fn collect_runtime_env() -> BTreeMap<String, String> {
    let mut runtime_env = BTreeMap::new();

    // Skill env：直接扫描磁盘（skills 已从 AgentConfig 脱离，由 skill plugin 自治）。
    let skills_root = default_skills_storage_dir_path();
    let view = scan_skill_registry(&skills_root);
    for entry in view.entries.values() {
        let manifest_path = entry.dir.join("skill.toml");
        let Ok(manifest) = read_skill_manifest(&manifest_path) else {
            continue;
        };
        if !manifest.available {
            continue;
        }
        for (key, value) in load_local_env(&entry.dir) {
            runtime_env.insert(key, value);
        }
    }

    runtime_env
}

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
