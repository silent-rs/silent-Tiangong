use std::fs;

use anyhow::{Context, Result, anyhow};

use super::common::{display_rel_path, resolve_workspace_write_path};
use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn write_file(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .first()
            .ok_or_else(|| anyhow!("write_file 缺少路径参数"))?;
        let content = call.args.get(1).cloned().unwrap_or_default();
        let full_path = resolve_workspace_write_path(path)?;
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        }
        fs::write(&full_path, content.as_bytes())
            .with_context(|| format!("写入文件失败：{}", full_path.display()))?;

        Ok(ToolResult {
            ok: true,
            summary: format!("文件写入成功：{}", display_rel_path(&full_path)),
            stdout: format!("written_bytes={}", content.len()),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}
