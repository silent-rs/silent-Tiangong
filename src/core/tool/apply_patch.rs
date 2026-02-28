use std::fs;

use anyhow::{Context, Result, anyhow};
use diffy::{Patch, apply as diffy_apply};

use super::common::{
    display_rel_path, resolve_workspace_path, resolve_workspace_write_path, truncate_output,
};
use super::{LocalToolExecutor, ToolCall, ToolResult};

const PATCH_BEGIN: &str = "*** Begin Patch";
const PATCH_END: &str = "*** End Patch";
const ADD_FILE_PREFIX: &str = "*** Add File: ";
const DELETE_FILE_PREFIX: &str = "*** Delete File: ";
const UPDATE_FILE_PREFIX: &str = "*** Update File: ";
const MOVE_TO_PREFIX: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const CHUNK_PREFIX: &str = "@@";

#[derive(Debug)]
enum PatchOperation {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<PatchChunk>,
    },
}

#[derive(Debug)]
struct PatchChunk {
    lines: Vec<PatchLine>,
}

#[derive(Debug)]
enum PatchLine {
    Context(String),
    Add(String),
    Remove(String),
}

impl LocalToolExecutor {
    pub(super) fn apply_patch(&self, call: &ToolCall) -> Result<ToolResult> {
        let patch = call
            .args
            .first()
            .ok_or_else(|| anyhow!("apply_patch 缺少 patch 内容参数"))?;
        if patch.trim().is_empty() {
            return Err(anyhow!("apply_patch patch 内容不能为空"));
        }

        if patch.trim_start().starts_with(PATCH_BEGIN) {
            return apply_codex_style_patch(patch);
        }

        apply_unified_diff_patch(patch)
    }
}

fn apply_codex_style_patch(patch: &str) -> Result<ToolResult> {
    let operations = parse_codex_patch(patch)?;
    let mut applied = Vec::new();

    for operation in operations {
        match operation {
            PatchOperation::Add { path, lines } => {
                let full_path = resolve_workspace_write_path(&path)?;
                if full_path.exists() {
                    return Err(anyhow!("Add File 目标已存在：{}", full_path.display()));
                }
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("创建目录失败：{}", parent.display()))?;
                }
                let content = lines.join("\n");
                fs::write(&full_path, content.as_bytes())
                    .with_context(|| format!("写入新增文件失败：{}", full_path.display()))?;
                applied.push(format!("ADD {}", display_rel_path(&full_path)));
            }
            PatchOperation::Delete { path } => {
                let full_path = resolve_workspace_path(&path)?;
                if !full_path.is_file() {
                    return Err(anyhow!("Delete File 目标不是文件：{}", full_path.display()));
                }
                fs::remove_file(&full_path)
                    .with_context(|| format!("删除文件失败：{}", full_path.display()))?;
                applied.push(format!("DELETE {}", display_rel_path(&full_path)));
            }
            PatchOperation::Update {
                path,
                move_to,
                chunks,
            } => {
                let source_path = resolve_workspace_path(&path)?;
                if !source_path.is_file() {
                    return Err(anyhow!(
                        "Update File 目标不是文件：{}",
                        source_path.display()
                    ));
                }
                let original = fs::read_to_string(&source_path)
                    .with_context(|| format!("读取文件失败：{}", source_path.display()))?;
                let trailing_newline = original.ends_with('\n');
                let mut lines = split_content_lines(&original);
                let mut cursor = 0usize;

                for chunk in &chunks {
                    apply_chunk(&mut lines, chunk, &mut cursor)?;
                }

                let mut next_content = lines.join("\n");
                if trailing_newline {
                    next_content.push('\n');
                }

                let target_path = if let Some(target) = move_to.as_ref() {
                    resolve_workspace_write_path(target)?
                } else {
                    source_path.clone()
                };

                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("创建目录失败：{}", parent.display()))?;
                }
                fs::write(&target_path, next_content.as_bytes())
                    .with_context(|| format!("写入文件失败：{}", target_path.display()))?;

                if target_path != source_path {
                    fs::remove_file(&source_path)
                        .with_context(|| format!("删除原文件失败：{}", source_path.display()))?;
                    applied.push(format!(
                        "UPDATE {} -> {}",
                        display_rel_path(&source_path),
                        display_rel_path(&target_path)
                    ));
                } else {
                    applied.push(format!("UPDATE {}", display_rel_path(&source_path)));
                }
            }
        }
    }

    Ok(ToolResult {
        ok: true,
        summary: format!("补丁应用成功：{} 项变更", applied.len()),
        stdout: truncate_output(&applied.join("\n")),
        stderr: String::new(),
        exit_code: 0,
        execution: None,
    })
}

fn apply_unified_diff_patch(patch: &str) -> Result<ToolResult> {
    let sections = split_unified_diff_sections(patch)?;
    let mut applied = Vec::new();

    for section in &sections {
        let parsed = Patch::from_str(section).context("解析 unified diff 失败")?;
        let original = normalize_diff_filename(parsed.original().unwrap_or_default())?;
        let modified = normalize_diff_filename(parsed.modified().unwrap_or_default())?;

        let is_add = original == "/dev/null" && modified != "/dev/null";
        let is_delete = modified == "/dev/null" && original != "/dev/null";

        if is_add {
            let target = resolve_workspace_write_path(&modified)?;
            if target.exists() {
                return Err(anyhow!("unified diff 新增文件已存在：{}", target.display()));
            }
            let content =
                diffy_apply("", &parsed).with_context(|| format!("应用补丁失败：{modified}"))?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("创建目录失败：{}", parent.display()))?;
            }
            fs::write(&target, content.as_bytes())
                .with_context(|| format!("写入新增文件失败：{}", target.display()))?;
            applied.push(format!("ADD {}", display_rel_path(&target)));
            continue;
        }

        if is_delete {
            let source = resolve_workspace_path(&original)?;
            if !source.is_file() {
                return Err(anyhow!(
                    "unified diff 删除目标不是文件：{}",
                    source.display()
                ));
            }
            let base = fs::read_to_string(&source)
                .with_context(|| format!("读取文件失败：{}", source.display()))?;
            let content =
                diffy_apply(&base, &parsed).with_context(|| format!("应用补丁失败：{original}"))?;
            if !content.is_empty() {
                return Err(anyhow!(
                    "unified diff 删除失败：应用后内容非空，拒绝删除：{}",
                    source.display()
                ));
            }
            fs::remove_file(&source)
                .with_context(|| format!("删除文件失败：{}", source.display()))?;
            applied.push(format!("DELETE {}", display_rel_path(&source)));
            continue;
        }

        let source_path_text = if original == "/dev/null" {
            modified.as_str()
        } else {
            original.as_str()
        };
        let target_path_text = if modified == "/dev/null" {
            original.as_str()
        } else {
            modified.as_str()
        };

        let source = resolve_workspace_path(source_path_text)?;
        if !source.is_file() {
            return Err(anyhow!(
                "unified diff 修改目标不是文件：{}",
                source.display()
            ));
        }
        let target = resolve_workspace_write_path(target_path_text)?;
        let base = fs::read_to_string(&source)
            .with_context(|| format!("读取文件失败：{}", source.display()))?;
        let content = diffy_apply(&base, &parsed)
            .with_context(|| format!("应用补丁失败：{target_path_text}"))?;

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        }
        fs::write(&target, content.as_bytes())
            .with_context(|| format!("写入文件失败：{}", target.display()))?;

        if source != target {
            fs::remove_file(&source)
                .with_context(|| format!("删除原文件失败：{}", source.display()))?;
            applied.push(format!(
                "UPDATE {} -> {}",
                display_rel_path(&source),
                display_rel_path(&target)
            ));
        } else {
            applied.push(format!("UPDATE {}", display_rel_path(&target)));
        }
    }

    Ok(ToolResult {
        ok: true,
        summary: format!("补丁应用成功：{} 项变更", applied.len()),
        stdout: truncate_output(&applied.join("\n")),
        stderr: String::new(),
        exit_code: 0,
        execution: None,
    })
}

fn split_unified_diff_sections(patch: &str) -> Result<Vec<String>> {
    let lines = patch.lines().collect::<Vec<_>>();
    if lines.len() < 3 {
        return Err(anyhow!("unified diff 内容过短，无法解析"));
    }

    let mut section_starts = Vec::new();
    for idx in 0..(lines.len() - 1) {
        if lines[idx].starts_with("--- ") && lines[idx + 1].starts_with("+++ ") {
            section_starts.push(idx);
        }
    }
    if section_starts.is_empty() {
        return Err(anyhow!("unified diff 缺少文件头（--- / +++）"));
    }

    let mut sections = Vec::new();
    for (index, start) in section_starts.iter().enumerate() {
        let end = section_starts
            .get(index + 1)
            .copied()
            .unwrap_or(lines.len());
        let mut section = lines[*start..end].join("\n");
        if !section.ends_with('\n') {
            section.push('\n');
        }
        sections.push(section);
    }
    Ok(sections)
}

fn normalize_diff_filename(raw: &str) -> Result<String> {
    let path = raw.trim();
    if path.is_empty() {
        return Err(anyhow!("unified diff 文件路径为空"));
    }
    if path == "/dev/null" {
        return Ok(path.to_string());
    }
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .trim();
    if path.is_empty() {
        return Err(anyhow!("unified diff 文件路径非法"));
    }
    Ok(path.to_string())
}

fn parse_codex_patch(patch: &str) -> Result<Vec<PatchOperation>> {
    let lines = patch
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .collect::<Vec<_>>();
    if lines.is_empty() || lines[0].trim() != PATCH_BEGIN {
        return Err(anyhow!("补丁格式非法：缺少 *** Begin Patch"));
    }

    let mut idx = 1usize;
    let mut operations = Vec::new();
    let mut found_end = false;
    while idx < lines.len() {
        let line = lines[idx];
        if line.trim() == PATCH_END {
            found_end = true;
            break;
        }

        if let Some(path) = line.strip_prefix(ADD_FILE_PREFIX) {
            let path = parse_patch_path(path, "Add File")?;
            idx += 1;
            let mut add_lines = Vec::new();
            while idx < lines.len() {
                let current = lines[idx];
                if current.starts_with("*** ") || current.trim() == PATCH_END {
                    break;
                }
                let Some(content) = current.strip_prefix('+') else {
                    return Err(anyhow!("Add File 内容行必须以 '+' 开头：line={}", idx + 1));
                };
                add_lines.push(content.to_string());
                idx += 1;
            }
            operations.push(PatchOperation::Add {
                path,
                lines: add_lines,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix(DELETE_FILE_PREFIX) {
            let path = parse_patch_path(path, "Delete File")?;
            operations.push(PatchOperation::Delete { path });
            idx += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix(UPDATE_FILE_PREFIX) {
            let path = parse_patch_path(path, "Update File")?;
            idx += 1;
            let mut move_to = None;
            if idx < lines.len()
                && let Some(target) = lines[idx].strip_prefix(MOVE_TO_PREFIX)
            {
                move_to = Some(parse_patch_path(target, "Move to")?);
                idx += 1;
            }

            let mut chunks = Vec::new();
            let mut current_chunk = Vec::new();
            while idx < lines.len() {
                let current = lines[idx];
                if current == EOF_MARKER {
                    idx += 1;
                    continue;
                }
                if current.trim() == PATCH_END || current.starts_with("*** ") {
                    break;
                }
                if current.starts_with(CHUNK_PREFIX) {
                    if !current_chunk.is_empty() {
                        chunks.push(PatchChunk {
                            lines: std::mem::take(&mut current_chunk),
                        });
                    }
                    idx += 1;
                    continue;
                }
                if let Some(content) = current.strip_prefix(' ') {
                    current_chunk.push(PatchLine::Context(content.to_string()));
                    idx += 1;
                    continue;
                }
                if let Some(content) = current.strip_prefix('+') {
                    current_chunk.push(PatchLine::Add(content.to_string()));
                    idx += 1;
                    continue;
                }
                if let Some(content) = current.strip_prefix('-') {
                    current_chunk.push(PatchLine::Remove(content.to_string()));
                    idx += 1;
                    continue;
                }
                return Err(anyhow!(
                    "Update File 内容行非法，必须以 ' ' / '+' / '-' / '@@' 开头：line={}",
                    idx + 1
                ));
            }
            if !current_chunk.is_empty() {
                chunks.push(PatchChunk {
                    lines: current_chunk,
                });
            }
            if chunks.is_empty() && move_to.is_none() {
                return Err(anyhow!("Update File 未包含有效修改内容：path={path}"));
            }
            operations.push(PatchOperation::Update {
                path,
                move_to,
                chunks,
            });
            continue;
        }

        return Err(anyhow!("未知补丁指令：line={} content={}", idx + 1, line));
    }

    if !found_end {
        return Err(anyhow!("补丁格式非法：缺少 *** End Patch"));
    }
    if operations.is_empty() {
        return Err(anyhow!("补丁内容为空：未解析到任何变更"));
    }

    Ok(operations)
}

fn parse_patch_path(raw: &str, label: &str) -> Result<String> {
    let path = raw.trim();
    if path.is_empty() {
        return Err(anyhow!("{label} 路径不能为空"));
    }
    Ok(path.to_string())
}

fn split_content_lines(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    raw.lines().map(ToString::to_string).collect::<Vec<_>>()
}

fn apply_chunk(lines: &mut Vec<String>, chunk: &PatchChunk, cursor: &mut usize) -> Result<()> {
    let old_lines = chunk
        .lines
        .iter()
        .filter_map(|line| match line {
            PatchLine::Context(value) | PatchLine::Remove(value) => Some(value.clone()),
            PatchLine::Add(_) => None,
        })
        .collect::<Vec<_>>();
    let new_lines = chunk
        .lines
        .iter()
        .filter_map(|line| match line {
            PatchLine::Context(value) | PatchLine::Add(value) => Some(value.clone()),
            PatchLine::Remove(_) => None,
        })
        .collect::<Vec<_>>();

    let from_cursor = find_sequence(lines, &old_lines, *cursor);
    let start = from_cursor
        .or_else(|| find_sequence(lines, &old_lines, 0))
        .ok_or_else(|| anyhow!("Update File 匹配失败：无法定位待替换片段"))?;
    let end = start + old_lines.len();
    let next_len = new_lines.len();
    lines.splice(start..end, new_lines);
    *cursor = start + next_len;
    Ok(())
}

fn find_sequence(lines: &[String], pattern: &[String], start: usize) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start.min(lines.len()));
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let max_start = lines.len() - pattern.len();
    (start..=max_start).find(|idx| lines[*idx..*idx + pattern.len()] == *pattern)
}
