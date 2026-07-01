//! MCP / skills 环境变量收集（供子进程执行注入）。
//!
//! 原属 `tool/run_command.rs::collect_runtime_env`，LocalToolExecutor 删除后迁出至此，
//! 供 RuntimeEngine 在构造时收集、command 插件在 register 时读取。

use std::collections::BTreeMap;
use std::path::Path;

use crate::agent_config::AgentConfig;

/// 从 agent_config 的 MCP servers 和 skills 收集环境变量。
pub fn collect_runtime_env(agent_config: &AgentConfig) -> BTreeMap<String, String> {
    let mut runtime_env = BTreeMap::new();

    if agent_config.mcp.enabled {
        for server in &agent_config.mcp.servers {
            if !server.enabled {
                continue;
            }
            for (key, value) in &server.env {
                let key = key.trim();
                if !is_valid_env_key(key) {
                    continue;
                }
                runtime_env.insert(key.to_string(), value.trim().to_string());
            }
        }
    }

    if agent_config.skills.enabled {
        for skill in &agent_config.skills.installed {
            if !skill.enabled {
                continue;
            }
            let source = skill.source.value.trim();
            if source.is_empty() {
                continue;
            }
            let source_path = Path::new(source);
            let skill_dir = if source_path.is_dir() {
                source_path
            } else if let Some(parent) = source_path.parent() {
                parent
            } else {
                continue;
            };
            for (key, value) in load_local_env(skill_dir) {
                runtime_env.insert(key, value);
            }
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
