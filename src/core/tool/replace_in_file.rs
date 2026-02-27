use std::fs;

use anyhow::{Context, Result, anyhow};

use super::common::{display_rel_path, resolve_workspace_write_path};
use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn replace_in_file(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .first()
            .ok_or_else(|| anyhow!("replace_in_file 缺少路径参数"))?;
        let old = call
            .args
            .get(1)
            .ok_or_else(|| anyhow!("replace_in_file 缺少 old 参数"))?;
        let new = call
            .args
            .get(2)
            .ok_or_else(|| anyhow!("replace_in_file 缺少 new 参数"))?;
        if old.is_empty() {
            return Err(anyhow!("replace_in_file old 参数不能为空"));
        }

        let full_path = resolve_workspace_write_path(path)?;
        if !full_path.is_file() {
            return Err(anyhow!(
                "replace_in_file 目标不是文件：{}",
                full_path.display()
            ));
        }

        let content = fs::read_to_string(&full_path)
            .with_context(|| format!("读取文件失败：{}", full_path.display()))?;
        let count = content.matches(old).count();
        if count == 0 {
            return Err(anyhow!("replace_in_file 未找到待替换内容"));
        }

        let replaced = content.replace(old, new);
        fs::write(&full_path, replaced.as_bytes())
            .with_context(|| format!("写入替换结果失败：{}", full_path.display()))?;

        Ok(ToolResult {
            ok: true,
            summary: format!(
                "文件替换成功：{} (replacements={count})",
                display_rel_path(&full_path)
            ),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}
