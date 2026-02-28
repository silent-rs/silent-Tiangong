use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::process::Command;
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::time::timeout;

use super::common::{
    command_env_allowlist, command_timeout_ms, derive_shell_exec_args, is_allowed_command,
    resolve_effective_cwd, truncate_output, validate_command_args_in_allowed_roots,
    validate_shell_command_args,
};
use super::{LocalToolExecutor, ToolCall, ToolResult};

const INTERNAL_SHELL_CMD: &str = "__tiangong_shell__";
const INTERNAL_CWD_PREFIX: &str = "__tiangong_cwd=";

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

        let (cmd, args) = if raw_cmd == INTERNAL_SHELL_CMD {
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

        if matches!(cmd.as_str(), "bash" | "sh" | "powershell" | "pwsh") {
            validate_shell_command_args(&cmd, &args, &effective_cwd)?;
        } else {
            if !is_allowed_command(&cmd) {
                return Err(anyhow!("不允许执行命令：{cmd}"));
            }
            validate_command_args_in_allowed_roots(&cmd, &args, &effective_cwd)?;
        }

        let timeout_ms = command_timeout_ms();
        let env_allowlist = command_env_allowlist();
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
            timeout(Duration::from_millis(timeout_ms), command.output()).await
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
