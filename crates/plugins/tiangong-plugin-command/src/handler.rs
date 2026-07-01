//! run_command / run_shell / run_bash 工具规格与覆盖处理器。
//!
//! 从 core 的 tool/run_command.rs 迁入，改造点：
//! - 参数从位置参数改为命名参数（直接读 call.arguments JSON）
//! - 纯 tokio::process::Command 子进程执行（CLI/Server 无 PTY）
//! - 完整保留命令白名单 / 路径越界 / shell 脚本校验（复用 core common.rs）
//! - trust_mode=FullTrust 时跳过校验（与原 core 行为一致）

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool::common as shared;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};
use tokio::process::Command;
use tokio::time::timeout;

use crate::plugin::CommandPlugin;

const TOOL_RUN_COMMAND: &str = "run_command";
const TOOL_RUN_SHELL: &str = "run_shell";

impl CommandPlugin {
    fn base(&self) -> Result<std::path::PathBuf> {
        self.workspace()
            .ok_or_else(|| anyhow!("会话工作目录未注入，无法执行命令"))
    }

    /// 同步解析 + 校验，返回拥有 owned 数据的执行 Future（满足 ToolOverrideHandler 的 'static 约束）。
    fn dispatch_sync(
        &self,
        call: &ToolCall,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send>>> {
        let tool_name = call.name.clone();
        let result = match call.name.as_str() {
            TOOL_RUN_COMMAND => self.prepare_run_command(&call.arguments),
            TOOL_RUN_SHELL => self.prepare_run_shell(&call.arguments),
            "run_bash" => self.prepare_run_bash(&call.arguments),
            _ => return None,
        };
        Some(match result {
            Ok(future) => future,
            Err(e) => {
                let err = tool_error(&tool_name, e);
                Box::pin(async move { err })
            }
        })
    }

    fn prepare_run_command(
        &self,
        args: &Value,
    ) -> Result<std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send>>> {
        let base = self.base()?;
        let raw_cmd = args
            .get("cmd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if raw_cmd.is_empty() {
            return Err(anyhow!("run_command 缺少 cmd 参数"));
        }

        let (cmd, mut cmd_args) = split_command(&raw_cmd);
        if let Some(arr) = args.get("args").and_then(Value::as_array) {
            cmd_args.extend(
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string),
            );
        }
        let cwd = args
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let timeout_ms = args
            .get("timeout")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .filter(|v| *v > 0);

        let effective_cwd = shared::resolve_effective_cwd_with(cwd, &base)?;
        let timeout_ms = timeout_ms.unwrap_or_else(shared::command_timeout_ms);

        if !self.is_full_trust() {
            validate_command(&cmd, &cmd_args, &effective_cwd)?;
        }

        let runtime_env = self.runtime_env();
        Ok(Box::pin(async move {
            exec_and_collect(&cmd, &cmd_args, &effective_cwd, timeout_ms, &runtime_env)
                .await
                .unwrap_or_else(|e| tool_error("命令执行", e))
        }))
    }

    fn prepare_run_shell(
        &self,
        args: &Value,
    ) -> Result<std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send>>> {
        let base = self.base()?;
        let script = args
            .get("script")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if script.is_empty() {
            return Err(anyhow!("run_shell 缺少 script 参数"));
        }
        let shell = args.get("shell").and_then(Value::as_str).unwrap_or("auto");
        let cwd = args
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let timeout_ms = args
            .get("timeout")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .filter(|v| *v > 0);

        let (cmd, cmd_args) = shared::derive_shell_exec_args(&script, Some(shell))?;
        let effective_cwd = shared::resolve_effective_cwd_with(cwd, &base)?;
        let timeout_ms = timeout_ms.unwrap_or_else(shared::command_timeout_ms);

        if !self.is_full_trust() {
            shared::validate_shell_command_args(&cmd, &cmd_args, &effective_cwd)?;
        }

        let runtime_env = self.runtime_env();
        Ok(Box::pin(async move {
            exec_and_collect(&cmd, &cmd_args, &effective_cwd, timeout_ms, &runtime_env)
                .await
                .unwrap_or_else(|e| tool_error("命令执行", e))
        }))
    }

    fn prepare_run_bash(
        &self,
        args: &Value,
    ) -> Result<std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send>>> {
        let base = self.base()?;
        let script = args
            .get("script")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if script.is_empty() {
            return Err(anyhow!("run_bash 缺少 script 参数"));
        }
        let (cmd, cmd_args) = shared::derive_shell_exec_args(&script, Some("bash"))?;
        let effective_cwd = shared::resolve_effective_cwd_with(None, &base)?;
        let timeout_ms = shared::command_timeout_ms();

        if !self.is_full_trust() {
            shared::validate_shell_command_args(&cmd, &cmd_args, &effective_cwd)?;
        }

        let runtime_env = self.runtime_env();
        Ok(Box::pin(async move {
            exec_and_collect(&cmd, &cmd_args, &effective_cwd, timeout_ms, &runtime_env)
                .await
                .unwrap_or_else(|e| tool_error("命令执行", e))
        }))
    }
}

/// 校验命令（非 shell）：白名单 + 路径越界。
fn validate_command(cmd: &str, args: &[String], cwd: &Path) -> Result<()> {
    if matches!(cmd, "bash" | "sh" | "powershell" | "pwsh") {
        shared::validate_shell_command_args(cmd, args, cwd)?;
    } else {
        if !shared::is_allowed_command(cmd) {
            return Err(anyhow!("不允许执行命令：{cmd}"));
        }
        shared::validate_command_args_in_allowed_roots(cmd, args, cwd)?;
    }
    Ok(())
}

/// 拆分命令字符串为 (程序名, 参数列表)。
fn split_command(raw: &str) -> (String, Vec<String>) {
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

/// 加载 cwd 下的 .env.local / .env 文件。
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

/// 执行命令并收集输出。
async fn exec_and_collect(
    cmd: &str,
    args: &[String],
    cwd: &Path,
    timeout_ms: u64,
    runtime_env: &BTreeMap<String, String>,
) -> Result<ToolResult> {
    let env_allowlist = shared::command_env_allowlist();
    let file_env = load_local_env(cwd);

    let mut command = Command::new(cmd);
    tiangong_core::process::configure_tokio_no_window(&mut command);
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
    for (key, value) in runtime_env {
        command.env(key, value);
    }
    for (key, value) in &file_env {
        command.env(key, value);
    }

    let output_result = if timeout_ms > 0 {
        match timeout(Duration::from_millis(timeout_ms), command.output()).await {
            Ok(o) => o,
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

    Ok(ToolResult {
        ok,
        summary,
        stdout,
        stderr,
        exit_code,
        execution: None,
    })
}

fn tool_error(tool: &str, e: anyhow::Error) -> ToolResult {
    let summary = format!("{tool} 失败：{e}");
    ToolResult {
        ok: false,
        summary: summary.clone(),
        stdout: String::new(),
        stderr: summary,
        exit_code: 1,
        execution: None,
    }
}

impl ToolSpecProvider for CommandPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: TOOL_RUN_COMMAND.to_string(),
                description: "执行受控命令，支持 cwd 和超时设置。shell 脚本建议使用 run_shell"
                    .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "cmd": { "type": "string", "description": "命令名（可含参数，自动拆分）" },
                        "args": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "命令参数列表"
                        },
                        "cwd": { "type": "string", "description": "工作目录（可选）" },
                        "timeout": { "type": "integer", "description": "超时时间（秒），0 或不填表示不限时", "minimum": 0 }
                    },
                    "required": ["cmd"]
                }),
            },
            ToolSpec {
                name: TOOL_RUN_SHELL.to_string(),
                description: "执行 shell 脚本，自动派生 bash/sh/powershell 参数。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "script": { "type": "string", "description": "shell 脚本文本" },
                        "shell": { "type": "string", "description": "shell 类型：auto/bash/sh/powershell/pwsh，默认 auto" },
                        "cwd": { "type": "string", "description": "工作目录（可选）" },
                        "timeout": { "type": "integer", "description": "超时时间（秒），0 或不填表示不限时", "minimum": 0 }
                    },
                    "required": ["script"]
                }),
            },
        ]
    }
}

impl ToolOverrideHandler for CommandPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        _session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        // ToolOverrideHandler 要求返回 'static Future。先同步解析参数（借用 &self/&call），
        // 再用捕获的 owned 数据生成 Future，避免借用逃逸到 async 上下文。
        let future = match self.dispatch_sync(call) {
            Some(f) => f,
            None => return Box::pin(async { None }),
        };
        Box::pin(async move { Some(future.await) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiangong_core::core::Plugin;

    fn make_plugin(dir: &tempfile::TempDir) -> CommandPlugin {
        let plugin = CommandPlugin::new();
        plugin.set_workspace(dir.path());
        plugin
    }

    fn make_call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: "test".to_string(),
            name: name.to_string(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn run_command_echo() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let future = plugin
            .dispatch_sync(&make_call(TOOL_RUN_COMMAND, json!({ "cmd": "echo hello" })))
            .unwrap();
        let result = future.await;
        assert!(result.ok, "{}", result.summary);
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn disallowed_command_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let future = plugin
            .dispatch_sync(&make_call(
                TOOL_RUN_COMMAND,
                json!({ "cmd": "nslookup test" }),
            ))
            .unwrap();
        let result = future.await;
        assert!(!result.ok);
        assert!(result.summary.contains("不允许"));
    }

    #[tokio::test]
    async fn run_shell_echo() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let future = plugin
            .dispatch_sync(&make_call(
                TOOL_RUN_SHELL,
                json!({ "script": "echo shell_test" }),
            ))
            .unwrap();
        let result = future.await;
        assert!(result.ok, "{}", result.summary);
        assert!(result.stdout.contains("shell_test"));
    }

    #[tokio::test]
    async fn unknown_tool_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        assert!(
            plugin
                .dispatch_sync(&make_call("not_a_command_tool", json!({})))
                .is_none()
        );
    }
}
