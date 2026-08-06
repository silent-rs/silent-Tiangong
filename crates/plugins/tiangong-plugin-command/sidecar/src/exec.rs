//! 命令执行实现：tokio 子进程 spawn + kill_on_drop + env_clear + 三层 env 注入
//! + 超时 + stdout/stderr 截断。整体从原进程内 `handler.rs` 迁移。
//!
//! env 注入三层（对齐原实现）：
//! 1. allowlist（PATH/HOME 等 21 个系统/代理变量，从 sidecar 进程环境读）
//! 2. runtime_env（各插件贡献的汇总环境变量）
//! 3. file_env（cwd 下 .env.local / .env）
//!
//! 注：runtime_env 当前经 sidecar 进程环境继承（exec_env 由 host 注入 sidecar
//! 进程）。env_clear 后只回注这三类，不泄漏 sidecar 全部环境。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tiangong_plugin_command_protocol::exec::ExecResponse;
use tiangong_toolkit as shared;
use tokio::process::Command;
use tokio::time::timeout;

/// 执行命令并收集输出（搬迁自原 handler.rs 的 exec_and_collect）。
pub async fn exec_and_collect(
    cmd: &str,
    args: &[String],
    cwd: &Path,
    timeout_ms: u64,
) -> Result<ExecResponse> {
    let env_allowlist = shared::command_env_allowlist();
    // runtime_env：当前经 sidecar 进程环境继承。exec_env 由 host 在 spawn sidecar
    // 时注入（若 host 未注入则为空，与原进程内 runtime_env 恒空等价）。
    // TODO（沙箱预留点 C）：未来 runtime_env 接通后，此处应从受控来源读取，
    // 并过滤危险 key（LD_PRELOAD/DYLD_*/BASH_ENV/PATH 劫持等）。
    let runtime_env = collect_runtime_env();
    let file_env = load_local_env(cwd);

    let mut command = Command::new(cmd);
    shared::configure_tokio_no_window(&mut command);
    command.kill_on_drop(true);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for (key, value) in &env_allowlist {
        command.env(key, value);
    }
    for (key, value) in &runtime_env {
        command.env(key, value);
    }
    for (key, value) in &file_env {
        command.env(key, value);
    }

    let output_result = if timeout_ms > 0 {
        match timeout(Duration::from_millis(timeout_ms), command.output()).await {
            Ok(o) => o,
            Err(_) => {
                return Ok(ExecResponse {
                    ok: false,
                    summary: format!("命令执行超时：{cmd} (timeout_ms={timeout_ms})"),
                    exit_code: -1,
                    ..Default::default()
                });
            }
        }
    } else {
        command.output().await
    };

    let output = output_result.context(format!("执行命令失败：{cmd}"))?;
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = shared::truncate_output(&String::from_utf8_lossy(&output.stdout));
    let stderr = shared::truncate_output(&String::from_utf8_lossy(&output.stderr));
    let ok = output.status.success();
    let summary = if ok {
        format!("命令执行成功：{cmd}")
    } else {
        format!("命令执行失败：{cmd} (exit_code={exit_code})")
    };

    Ok(ExecResponse {
        ok,
        summary,
        stdout,
        stderr,
        exit_code,
    })
}

/// 拆分命令字符串为 (程序名, 参数列表)（搬迁自原 handler.rs）。
pub fn split_command(raw: &str) -> (String, Vec<String>) {
    let mut parts = Vec::new();
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
            '\\' if !in_single => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        return (raw.to_string(), Vec::new());
    }
    let cmd = parts.remove(0);
    (cmd, parts)
}

/// 收集 runtime_env：从 sidecar 进程环境读取各插件贡献的变量。
///
/// 当前 host 经 `TIANGONG_EXEC_ENV_*` 系列变量注入（若未注入则返回空）。
/// TODO：未来接通 exec_env 后改为受控来源 + 危险 key 过滤（沙箱预留点 C）。
fn collect_runtime_env() -> BTreeMap<String, String> {
    // 当前无插件贡献 exec_env（原进程内 runtime_env 恒空），返回空保持等价语义。
    // 未来 exec_env 接通后，host 会经环境变量或协议注入，此处读取并过滤危险 key。
    BTreeMap::new()
}

/// 加载 cwd 下的 .env.local / .env 文件（搬迁自原 handler.rs）。
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
