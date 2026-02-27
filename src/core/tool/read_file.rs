use std::fs;

use anyhow::{Context, Result, anyhow};

use super::common::{display_rel_path, resolve_workspace_path, truncate_output};
use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn read_file(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .first()
            .ok_or_else(|| anyhow!("read_file 缺少路径参数"))?;
        let full_path = resolve_workspace_path(path)?;
        if !full_path.is_file() {
            return Err(anyhow!("read_file 目标不是文件：{}", full_path.display()));
        }

        let content = fs::read(&full_path)
            .with_context(|| format!("读取文件失败：{}", full_path.display()))?;
        let stdout = String::from_utf8_lossy(&content).to_string();
        let stdout = truncate_output(&stdout);

        Ok(ToolResult {
            ok: true,
            summary: format!("已读取文件：{}", display_rel_path(&full_path)),
            stdout,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}
