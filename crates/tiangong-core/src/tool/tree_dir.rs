use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

use super::common::{
    display_rel_path, resolve_workspace_path, resolve_workspace_path_trusted,
};
use super::{LocalToolExecutor, ToolCall, ToolResult};

const DEFAULT_TREE_MAX_DEPTH: usize = 2;
const MAX_TREE_MAX_DEPTH: usize = 8;
const MAX_TREE_NODES: usize = 1200;

impl LocalToolExecutor {
    pub(super) fn tree_dir(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call.args.first().map_or(".", String::as_str);
        let max_depth = parse_tree_max_depth(call.args.get(1).map(String::as_str))?;
        let full_path = if self.is_full_trust() {
            resolve_workspace_path_trusted(path)?
        } else {
            resolve_workspace_path(path)?
        };
        if !full_path.is_dir() {
            return Err(anyhow!("tree_dir 目标不是目录：{}", full_path.display()));
        }

        let rel = display_rel_path(&full_path);
        let mut lines = vec![if rel == "." {
            "./".to_string()
        } else {
            format!("{rel}/")
        }];
        let mut visited = 0usize;
        let mut truncated = false;
        append_tree_lines(
            &full_path,
            0,
            max_depth,
            "",
            &mut lines,
            &mut visited,
            &mut truncated,
        )?;
        if truncated {
            lines.push(format!(
                "...(节点数量超过限制，已截断，max_nodes={MAX_TREE_NODES})"
            ));
        }

        Ok(ToolResult {
            ok: true,
            summary: format!(
                "目录树：{} (max_depth={max_depth})",
                display_rel_path(&full_path)
            ),
            stdout: lines.join("\n"),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}

fn parse_tree_max_depth(raw: Option<&str>) -> Result<usize> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(DEFAULT_TREE_MAX_DEPTH);
    };
    let parsed = text
        .parse::<usize>()
        .with_context(|| format!("tree_dir max_depth 参数非法：{text}"))?;
    if parsed > MAX_TREE_MAX_DEPTH {
        return Err(anyhow!(
            "tree_dir max_depth 不能超过 {}",
            MAX_TREE_MAX_DEPTH
        ));
    }
    Ok(parsed)
}

fn append_tree_lines(
    path: &Path,
    current_depth: usize,
    max_depth: usize,
    prefix: &str,
    lines: &mut Vec<String>,
    visited: &mut usize,
    truncated: &mut bool,
) -> Result<()> {
    if current_depth >= max_depth {
        return Ok(());
    }
    if *visited >= MAX_TREE_NODES {
        *truncated = true;
        return Ok(());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("读取目录失败：{}", path.display()))?
    {
        let entry = entry.with_context(|| format!("读取目录项失败：{}", path.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry
            .file_type()
            .with_context(|| format!("读取目录项类型失败：{}", path.display()))?;
        entries.push((name, file_type.is_dir(), entry.path()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let total = entries.len();
    for (idx, (name, is_dir, child_path)) in entries.into_iter().enumerate() {
        if *visited >= MAX_TREE_NODES {
            *truncated = true;
            return Ok(());
        }

        *visited += 1;
        let last = idx + 1 == total;
        let branch = if last { "`-- " } else { "|-- " };
        let display = if is_dir { format!("{name}/") } else { name };
        lines.push(format!("{prefix}{branch}{display}"));

        if is_dir {
            let next_prefix = if last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}|   ")
            };
            append_tree_lines(
                &child_path,
                current_depth + 1,
                max_depth,
                &next_prefix,
                lines,
                visited,
                truncated,
            )?;
            if *truncated {
                return Ok(());
            }
        }
    }

    Ok(())
}
