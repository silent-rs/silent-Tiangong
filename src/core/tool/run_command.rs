use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};

use super::common::{
    command_timeout_ms, execute_command_with_timeout, is_allowed_command, validate_bash_args,
    validate_command_args_in_allowed_roots, workspace_root,
};
use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn run_command(&self, call: &ToolCall) -> Result<ToolResult> {
        let cmd = call
            .args
            .first()
            .ok_or_else(|| anyhow!("run_command 缺少命令参数"))?;
        let args = call.args.iter().skip(1).cloned().collect::<Vec<_>>();

        if cmd == "bash" {
            validate_bash_args(&args)?;
        } else {
            if !is_allowed_command(cmd) {
                return Err(anyhow!("不允许执行命令：{cmd}"));
            }
            validate_command_args_in_allowed_roots(cmd, &args)?;
        }

        let timeout_ms = command_timeout_ms();
        let (output, timed_out) = execute_command_with_timeout(
            Command::new(cmd)
                .args(&args)
                .current_dir(workspace_root()?)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        )
        .with_context(|| format!("执行命令失败：{cmd}"))?;

        let mut exit_code = output.status.code().unwrap_or(-1);
        let stdout = super::common::truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = super::common::truncate_output(&String::from_utf8_lossy(&output.stderr));
        let ok = output.status.success() && !timed_out;

        let summary = if timed_out {
            exit_code = -1;
            format!("命令执行超时：{cmd} (timeout_ms={timeout_ms})")
        } else if ok {
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
