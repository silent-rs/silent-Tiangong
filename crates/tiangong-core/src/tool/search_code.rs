use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};

use super::common::{
    command_timeout_ms, execute_command_with_timeout, resolve_workspace_path,
    resolve_workspace_path_trusted, truncate_output, workspace_root,
};
use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn search_code(&self, call: &ToolCall) -> Result<ToolResult> {
        let pattern = call
            .args
            .first()
            .ok_or_else(|| anyhow!("search_code 缺少 pattern 参数"))?
            .trim();
        if pattern.is_empty() {
            return Err(anyhow!("search_code pattern 不能为空"));
        }

        let target = call.args.get(1).map_or(".", String::as_str);
        let full_path = if self.is_full_trust() {
            resolve_workspace_path_trusted(target)?
        } else {
            resolve_workspace_path(target)?
        };
        let timeout_ms = command_timeout_ms();
        let target_text = full_path.display().to_string();

        let rg_result = execute_command_with_timeout(
            Command::new("rg")
                .arg("--line-number")
                .arg("--no-heading")
                .arg("--color")
                .arg("never")
                .arg(pattern)
                .arg(&target_text)
                .current_dir(workspace_root()?)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        );

        let (output, timed_out) = match rg_result {
            Ok(payload) => payload,
            Err(_) => execute_command_with_timeout(
                Command::new("grep")
                    .arg("-R")
                    .arg("-n")
                    .arg(pattern)
                    .arg(&target_text)
                    .current_dir(workspace_root()?)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
                timeout_ms,
            )
            .with_context(|| format!("执行代码检索失败：pattern={pattern}"))?,
        };

        let exit_code = if timed_out {
            -1
        } else {
            output.status.code().unwrap_or(-1)
        };
        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
        let ok = !timed_out && (output.status.success() || exit_code == 1);
        let summary = if timed_out {
            format!("代码检索超时：pattern={pattern} (timeout_ms={timeout_ms})")
        } else if exit_code == 1 {
            format!("代码检索完成：未找到匹配（pattern={pattern}）")
        } else if ok {
            format!("代码检索成功：pattern={pattern}")
        } else {
            format!("代码检索失败：pattern={pattern} (exit_code={exit_code})")
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
