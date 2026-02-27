use std::fs;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};

use super::common::{
    command_timeout_ms, execute_command_with_timeout, truncate_output, workspace_root,
    write_temp_patch_file,
};
use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn apply_patch(&self, call: &ToolCall) -> Result<ToolResult> {
        let patch = call
            .args
            .first()
            .ok_or_else(|| anyhow!("apply_patch 缺少 patch 内容参数"))?;
        if patch.trim().is_empty() {
            return Err(anyhow!("apply_patch patch 内容不能为空"));
        }

        let temp_patch = write_temp_patch_file(patch)?;
        let timeout_ms = command_timeout_ms();
        let apply_result = execute_command_with_timeout(
            Command::new("git")
                .arg("apply")
                .arg("--whitespace=nowarn")
                .arg("--recount")
                .arg("--unidiff-zero")
                .arg(&temp_patch)
                .current_dir(workspace_root()?)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        );
        let _ = fs::remove_file(&temp_patch);

        let (output, timed_out) = apply_result.context("执行补丁应用失败")?;
        let exit_code = if timed_out {
            -1
        } else {
            output.status.code().unwrap_or(-1)
        };
        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
        let ok = !timed_out && output.status.success();
        let summary = if timed_out {
            format!("补丁应用超时 (timeout_ms={timeout_ms})")
        } else if ok {
            "补丁应用成功".to_string()
        } else {
            format!("补丁应用失败 (exit_code={exit_code})")
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
