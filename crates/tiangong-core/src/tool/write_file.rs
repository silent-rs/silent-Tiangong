use std::fs::{self, OpenOptions};

use anyhow::{Context, Result, anyhow};

use super::common::{display_rel_path, resolve_workspace_write_path, resolve_workspace_write_path_trusted};
use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn write_file(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .first()
            .ok_or_else(|| anyhow!("write_file 缺少路径参数"))?;
        let content = call.args.get(1).cloned().unwrap_or_default();
        let append = parse_bool_flag(call.args.get(2).map(String::as_str), false)?;
        let full_path = if self.is_full_trust() { resolve_workspace_write_path_trusted(path)? } else { resolve_workspace_write_path(path)? };
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        }
        if append {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&full_path)
                .with_context(|| format!("追加打开文件失败：{}", full_path.display()))?;
            use std::io::Write as _;
            file.write_all(content.as_bytes())
                .with_context(|| format!("追加写入文件失败：{}", full_path.display()))?;
        } else {
            atomic_write_file(&full_path, content.as_bytes())?;
        }

        Ok(ToolResult {
            ok: true,
            summary: format!(
                "文件写入成功：{} (mode={})",
                display_rel_path(&full_path),
                if append { "append" } else { "overwrite-atomic" }
            ),
            stdout: format!("written_bytes={},append={append}", content.len()),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}

fn atomic_write_file(path: &std::path::Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("无法确定目标文件父目录：{}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("目标文件名非法：{}", path.display()))?;
    let temp_path = parent.join(format!(".{}.tmp-{}", file_name, scru128::new()));
    fs::write(&temp_path, content)
        .with_context(|| format!("写入临时文件失败：{}", temp_path.display()))?;
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("删除旧文件失败：{}", path.display()))?;
    }
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "原子替换失败：temp={}, target={}",
            temp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn parse_bool_flag(raw: Option<&str>, default: bool) -> Result<bool> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(default);
    };
    match text.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(anyhow!("布尔参数非法：{text}")),
    }
}
