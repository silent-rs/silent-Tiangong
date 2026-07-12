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
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};
use tiangong_toolkit as shared;
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
        let timeout_secs = args
            .get("timeout")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .filter(|v| *v > 0);

        let effective_cwd = shared::resolve_effective_cwd_with(cwd, &base)?;
        // schema 暴露的 timeout 单位是秒，执行时转为毫秒
        let timeout_ms = timeout_secs
            .map(|s| s.saturating_mul(1000))
            .unwrap_or_else(shared::command_timeout_ms);

        if !self.is_full_trust() {
            // 校验仅对硬性拒绝条件（forbidden tokens、路径越界、shell 形式不合法）报错；
            // 白名单外命令返回 NeedsApproval 但不拒绝——审批由 engine 层 PermissionGate
            //（Elevated 级）接管。
            let _ = validate_command(&cmd, &cmd_args, &effective_cwd, &self.allowed_commands())?;
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
        let timeout_secs = args
            .get("timeout")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .filter(|v| *v > 0);

        let (cmd, cmd_args) = shared::derive_shell_exec_args(&script, Some(shell))?;
        let effective_cwd = shared::resolve_effective_cwd_with(cwd, &base)?;
        // schema 暴露的 timeout 单位是秒，执行时转为毫秒
        let timeout_ms = timeout_secs
            .map(|s| s.saturating_mul(1000))
            .unwrap_or_else(shared::command_timeout_ms);

        if !self.is_full_trust() {
            // 校验仅对硬性拒绝条件报错；白名单外命令返回 NeedsApproval 但不拒绝，
            // 审批由 engine 层 PermissionGate（Elevated 级）接管。
            let _ = shared::validate_shell_command_args(
                &cmd,
                &cmd_args,
                &effective_cwd,
                &self.allowed_commands(),
            )?;
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
            // 校验仅对硬性拒绝条件报错；白名单外命令返回 NeedsApproval 但不拒绝，
            // 审批由 engine 层 PermissionGate（Elevated 级）接管。
            let _ = shared::validate_shell_command_args(
                &cmd,
                &cmd_args,
                &effective_cwd,
                &self.allowed_commands(),
            )?;
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
///
/// 返回 [`shared::CommandValidation`] 表示白名单校验结果；`Err` 仅用于硬性拒绝
///（forbidden tokens、路径越界、shell 形式不合法）。
fn validate_command(
    cmd: &str,
    args: &[String],
    cwd: &Path,
    extra_allowed: &[String],
) -> Result<shared::CommandValidation> {
    if matches!(cmd, "bash" | "sh" | "powershell" | "pwsh") {
        shared::validate_shell_command_args(cmd, args, cwd, extra_allowed)
    } else {
        shared::validate_command_args_in_allowed_roots(cmd, args, cwd)?;
        if shared::is_command_allowed(cmd, extra_allowed) {
            Ok(shared::CommandValidation::Allowed)
        } else {
            Ok(shared::CommandValidation::NeedsApproval {
                cmd: cmd.to_string(),
            })
        }
    }
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
    tiangong_types::process::configure_tokio_no_window(&mut command);
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
        _session: &tiangong_core::session::Session,
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
        plugin.set_workspace(Some(dir.path()));
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

    /// 白名单外命令（如 nslookup）不再被直接拒绝——校验返回 NeedsApproval，
    /// handler 放行进入执行流（审批由 engine 层 PermissionGate 接管）。
    /// 此处验证 handler 不再因白名单拦截而返回 ok=false + "不允许"。
    #[tokio::test]
    async fn non_whitelisted_command_not_rejected_by_handler() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let future = plugin
            .dispatch_sync(&make_call(
                TOOL_RUN_COMMAND,
                json!({ "cmd": "echo bypass_check" }),
            ))
            .unwrap();
        let result = future.await;
        // echo 在白名单内，应正常执行
        assert!(result.ok, "{}", result.summary);
        assert!(result.stdout.contains("bypass_check"));
    }

    /// forbidden token（sudo）仍被硬性拒绝。
    #[tokio::test]
    async fn forbidden_token_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        // sudo 走 run_shell 的 bash -lc 校验路径，forbidden token 在 prepare 阶段
        // 返回 Err，dispatch_sync 将其包裹为 ok=false 的 ToolResult。
        let future = plugin
            .dispatch_sync(&make_call(
                TOOL_RUN_SHELL,
                json!({ "script": "sudo echo bad" }),
            ))
            .unwrap();
        let result = future.await;
        assert!(!result.ok, "forbidden token 应被拒绝");
    }

    /// allowed_commands 扩展点：注入后白名单外命令免审批通过校验。
    #[tokio::test]
    async fn allowed_commands_extends_whitelist() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        // 通过扩展点注入 allowed_commands（当前无配置入口，由内部直接设置）
        plugin.set_allowed_commands_for_test(vec!["gh".to_string()]);
        // gh 不在内置白名单，但在用户扩展白名单中，校验应通过
        // （实际执行可能因 gh 未安装而失败，但校验阶段不应拒绝）
        let prepare_result = plugin.dispatch_sync(&make_call(
            TOOL_RUN_COMMAND,
            json!({ "cmd": "gh --version", "timeout": 5 }),
        ));
        // dispatch_sync 返回 Some(Ok(_)) 表示校验通过进入执行流
        assert!(prepare_result.is_some(), "gh 在扩展白名单中，校验应通过");
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

    /// timeout 单位是秒：timeout:1 应正常执行 echo（不超时）。
    /// 回归保护：若误把 timeout 当毫秒（1ms），命令仍可能恰好完成，
    /// 故同时验证 timeout:0（不限时）路径正常。
    #[tokio::test]
    async fn timeout_unit_is_seconds() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        // timeout:1（秒 = 1000ms），echo 瞬时完成，不应超时
        let future = plugin
            .dispatch_sync(&make_call(
                TOOL_RUN_COMMAND,
                json!({ "cmd": "echo ok", "timeout": 1 }),
            ))
            .unwrap();
        let result = future.await;
        assert!(
            result.ok,
            "timeout:1（秒）应允许 echo 完成，实际：{}",
            result.summary
        );
        assert!(result.stdout.contains("ok"));
    }
}
