use std::fs;

use anyhow::{Context, Result, anyhow};

use super::common::{display_rel_path, resolve_workspace_path, resolve_workspace_path_trusted, truncate_output};
use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn list_dir(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call.args.first().map_or(".", String::as_str);
        let full_path = if self.is_full_trust() { resolve_workspace_path_trusted(path)? } else { resolve_workspace_path(path)? };
        if !full_path.is_dir() {
            return Err(anyhow!("list_dir 目标不是目录：{}", full_path.display()));
        }

        let mut items = Vec::new();
        for entry in fs::read_dir(&full_path)
            .with_context(|| format!("读取目录失败：{}", full_path.display()))?
        {
            let entry =
                entry.with_context(|| format!("读取目录项失败：{}", full_path.display()))?;
            let file_type = entry
                .file_type()
                .with_context(|| format!("读取目录项类型失败：{}", full_path.display()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let display = if file_type.is_dir() {
                format!("{name}/")
            } else {
                name
            };
            items.push(display);
        }
        items.sort();

        Ok(ToolResult {
            ok: true,
            summary: format!("目录列表：{}", display_rel_path(&full_path)),
            stdout: truncate_output(&items.join("\n")),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}
