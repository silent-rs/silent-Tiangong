use std::fs;

use anyhow::{Context, Result, anyhow};

use super::common::{display_rel_path, resolve_workspace_path, resolve_workspace_path_trusted};
use super::{LocalToolExecutor, ToolCall, ToolResult};

const DEFAULT_START_LINE: usize = 1;
const DEFAULT_MAX_LINES: usize = 200;
const MAX_MAX_LINES: usize = 2000;

impl LocalToolExecutor {
    pub(super) fn read_file(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .first()
            .ok_or_else(|| anyhow!("read_file 缺少路径参数"))?;
        let start_line = parse_usize_arg(
            call.args.get(1).map(String::as_str),
            "start_line",
            DEFAULT_START_LINE,
        )?;
        let max_lines = parse_usize_arg(
            call.args.get(2).map(String::as_str),
            "max_lines",
            DEFAULT_MAX_LINES,
        )?;
        if max_lines == 0 || max_lines > MAX_MAX_LINES {
            return Err(anyhow!(
                "read_file max_lines 范围非法：{}（允许 1..={}）",
                max_lines,
                MAX_MAX_LINES
            ));
        }
        let full_path = if self.is_full_trust() {
            resolve_workspace_path_trusted(path)?
        } else {
            resolve_workspace_path(path)?
        };
        if !full_path.is_file() {
            return Err(anyhow!("read_file 目标不是文件：{}", full_path.display()));
        }

        let content = fs::read_to_string(&full_path)
            .with_context(|| format!("读取文件失败：{}", full_path.display()))?;
        let lines = content.lines().collect::<Vec<_>>();
        let total_lines = lines.len();
        let start_idx = start_line.saturating_sub(1).min(total_lines);
        let end_idx = (start_idx + max_lines).min(total_lines);
        let selected = lines[start_idx..end_idx]
            .iter()
            .enumerate()
            .map(|(idx, line)| format!("{:>6}\t{}", start_idx + idx + 1, line))
            .collect::<Vec<_>>()
            .join("\n");
        // 文件内容完整返回给 LLM，不截断（截断由 LLM 通过 start_line/max_lines 参数控制）
        let stdout = selected;

        Ok(ToolResult {
            ok: true,
            summary: format!(
                "已读取文件：{} (range={}..{}, total_lines={})",
                display_rel_path(&full_path),
                start_idx + 1,
                end_idx,
                total_lines
            ),
            stdout,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}

fn parse_usize_arg(raw: Option<&str>, label: &str, default: usize) -> Result<usize> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(default);
    };
    text.parse::<usize>()
        .with_context(|| format!("read_file {label} 参数非法：{text}"))
}
