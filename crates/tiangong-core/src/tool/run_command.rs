use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::process::Command;
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::time::timeout;

use crate::agent_config::AgentConfig;

use super::common::{
    command_env_allowlist, command_timeout_ms, derive_shell_exec_args, is_allowed_command,
    resolve_effective_cwd, truncate_output, validate_command_args_in_allowed_roots,
    validate_shell_command_args,
};
use super::{LocalToolExecutor, ToolCall, ToolResult};

const INTERNAL_SHELL_CMD: &str = "__tiangong_shell__";
const INTERNAL_CWD_PREFIX: &str = "__tiangong_cwd=";

pub(super) fn collect_runtime_env(agent_config: &AgentConfig) -> BTreeMap<String, String> {
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

impl LocalToolExecutor {
    pub(super) fn run_command(&self, call: &ToolCall) -> Result<ToolResult> {
        let raw_cmd = call
            .args
            .first()
            .ok_or_else(|| anyhow!("run_command 缺少命令参数"))?
            .to_string();
        let mut raw_args = call.args.iter().skip(1).cloned().collect::<Vec<_>>();
        let cwd = extract_cwd_meta(&mut raw_args);
        let effective_cwd = resolve_effective_cwd(cwd.as_deref())?;

        let (cmd, mut args) = if raw_cmd == INTERNAL_SHELL_CMD {
            let script = raw_args
                .first()
                .map(String::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| anyhow!("run_shell 缺少 script 参数"))?;
            let shell = raw_args.get(1).map(String::as_str);
            derive_shell_exec_args(script, shell)?
        } else {
            (raw_cmd.clone(), raw_args)
        };

        // 提取 LLM 指定的超时（在 validate 之前，避免被当成路径参数）
        let timeout_ms = extract_timeout_meta(&mut args).unwrap_or_else(command_timeout_ms);

        if matches!(cmd.as_str(), "bash" | "sh" | "powershell" | "pwsh") {
            validate_shell_command_args(&cmd, &args, &effective_cwd)?;
        } else {
            if !is_allowed_command(&cmd) {
                return Err(anyhow!("不允许执行命令：{cmd}"));
            }
            validate_command_args_in_allowed_roots(&cmd, &args, &effective_cwd)?;
        }
        let env_allowlist = command_env_allowlist();
        let runtime_env = self.runtime_env();
        let file_env = load_local_env(&effective_cwd);
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化命令执行运行时失败")?;
        let output = runtime.block_on(async {
            let mut command = Command::new(&cmd);
            command
                .args(&args)
                .current_dir(&effective_cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env_clear();
            for (key, value) in &env_allowlist {
                command.env(key, value);
            }
            for (key, value) in runtime_env {
                command.env(key, value);
            }
            for (key, value) in &file_env {
                command.env(key, value);
            }
            if timeout_ms > 0 {
                timeout(Duration::from_millis(timeout_ms), command.output()).await
            } else {
                // timeout_ms=0 表示不设超时，直接等待完成
                Ok(command.output().await)
            }
        });

        let (output, timed_out) = match output {
            Ok(Ok(payload)) => (payload, false),
            Ok(Err(err)) => return Err(anyhow!("执行命令失败：{cmd}，{err}")),
            Err(_) => {
                return Ok(ToolResult {
                    ok: false,
                    summary: format!("命令执行超时：{cmd} (timeout_ms={timeout_ms})"),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: -1,
                    execution: None,
                });
            }
        };

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
        let ok = output.status.success() && !timed_out;
        let summary = if ok {
            format!("命令执行成功：{cmd}")
        } else {
            format!("命令执行失败：{cmd} (exit_code={exit_code})")
        };

        Ok(ToolResult {
            ok,
            summary,
            stdout,
            stderr,
            exit_code,
            execution: None,
        })
    }
}

fn extract_cwd_meta(args: &mut Vec<String>) -> Option<String> {
    let mut cwd = None;
    args.retain(|arg| {
        if let Some(value) = arg.strip_prefix(INTERNAL_CWD_PREFIX) {
            cwd = Some(value.to_string());
            false
        } else {
            true
        }
    });
    cwd
}

fn load_local_env(cwd: &Path) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for file in [".env.local", ".env"] {
        let path = cwd.join(file);
        if !path.is_file() {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
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

const INTERNAL_TIMEOUT_PREFIX: &str = "__tiangong_timeout=";

/// 从参数列表中提取 __tiangong_timeout=N 元数据，返回超时毫秒数
fn extract_timeout_meta(args: &mut Vec<String>) -> Option<u64> {
    let idx = args.iter().position(|a| a.starts_with(INTERNAL_TIMEOUT_PREFIX))?;
    let raw = args.remove(idx);
    raw[INTERNAL_TIMEOUT_PREFIX.len()..].trim().parse::<u64>().ok()
}
