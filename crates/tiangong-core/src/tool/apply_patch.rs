use std::fs;
use std::path::Path;

use anyhow::{Result, anyhow};
use diffy::{Patch, apply as diffy_apply};
use serde_json::json;

use super::common::{
    display_rel_path, resolve_effective_cwd, resolve_write_path_from_base, truncate_output,
};
use super::{LocalToolExecutor, ToolCall, ToolResult};

#[derive(Debug, Default)]
struct PatchStats {
    added: usize,
    deleted: usize,
    updated: usize,
    moved: usize,
    files: Vec<serde_json::Value>,
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

        let verify = parse_bool_arg(call.args.get(1).map(String::as_str), false)?;
        let effective_cwd = resolve_effective_cwd(call.args.get(2).map(String::as_str))
            .map_err(|err| patch_path_error(err.to_string()))?;
        let stats = apply_unified_diff_patch(patch, &effective_cwd, verify)?;
        let summary = format!(
            "补丁{}成功：add={}, delete={}, update={}, move={}",
            if verify { "校验" } else { "应用" },
            stats.added,
            stats.deleted,
            stats.updated,
            stats.moved
        );
        let stdout = json!({
            "verify": verify,
            "effective_cwd": effective_cwd.display().to_string(),
            "counts": {
                "add": stats.added,
                "delete": stats.deleted,
                "update": stats.updated,
                "move": stats.moved,
            },
            "files": stats.files,
        })
        .to_string();

        Ok(ToolResult {
            ok: true,
            summary,
            stdout: truncate_output(&stdout),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}

fn apply_unified_diff_patch(patch: &str, effective_cwd: &Path, verify: bool) -> Result<PatchStats> {
    let sections = split_unified_diff_sections(patch)?;
    let mut stats = PatchStats::default();

    for section in &sections {
        let parsed = Patch::from_str(section)
            .map_err(|err| patch_parse_error(format!("解析 unified diff 失败：{err}")))?;
        let original = normalize_diff_filename(parsed.original().unwrap_or_default())?;
        let modified = normalize_diff_filename(parsed.modified().unwrap_or_default())?;

        let is_add = original == "/dev/null" && modified != "/dev/null";
        let is_delete = modified == "/dev/null" && original != "/dev/null";

        if is_add {
            let target = resolve_write_path_from_base(&modified, effective_cwd)
                .map_err(|err| patch_path_error(err.to_string()))?;
            if target.exists() {
                return Err(patch_path_error(format!(
                    "新增文件已存在：{}",
                    target.display()
                )));
            }
            let content = diffy_apply("", &parsed)
                .map_err(|err| patch_content_error(format!("新增文件补丁应用失败：{err}")))?;
            if !verify {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|err| {
                        patch_write_error(format!("创建目录失败：{}，{err}", parent.display()))
                    })?;
                }
                fs::write(&target, content.as_bytes()).map_err(|err| {
                    patch_write_error(format!("写入新增文件失败：{}，{err}", target.display()))
                })?;
            }
            stats.added += 1;
            stats.files.push(json!({
                "action": "add",
                "target_rel": display_rel_path(&target),
                "target_abs": target.display().to_string(),
            }));
            continue;
        }

        if is_delete {
            let source = resolve_write_path_from_base(&original, effective_cwd)
                .map_err(|err| patch_path_error(err.to_string()))?;
            if !source.is_file() {
                return Err(patch_path_error(format!(
                    "删除目标不是文件：{}",
                    source.display()
                )));
            }
            let base = fs::read_to_string(&source).map_err(|err| {
                patch_write_error(format!("读取删除目标失败：{}，{err}", source.display()))
            })?;
            let content = diffy_apply(&base, &parsed)
                .map_err(|err| patch_content_error(format!("删除补丁应用失败：{err}")))?;
            if !content.is_empty() {
                return Err(patch_content_error(format!(
                    "删除补丁校验失败：应用后内容非空：{}",
                    source.display()
                )));
            }
            if !verify {
                fs::remove_file(&source).map_err(|err| {
                    patch_write_error(format!("删除文件失败：{}，{err}", source.display()))
                })?;
            }
            stats.deleted += 1;
            stats.files.push(json!({
                "action": "delete",
                "source_rel": display_rel_path(&source),
                "source_abs": source.display().to_string(),
            }));
            continue;
        }

        let source = resolve_write_path_from_base(&original, effective_cwd)
            .map_err(|err| patch_path_error(err.to_string()))?;
        let target = resolve_write_path_from_base(&modified, effective_cwd)
            .map_err(|err| patch_path_error(err.to_string()))?;
        if !source.is_file() {
            return Err(patch_path_error(format!(
                "修改目标不是文件：{}",
                source.display()
            )));
        }
        let base = fs::read_to_string(&source).map_err(|err| {
            patch_write_error(format!("读取文件失败：{}，{err}", source.display()))
        })?;
        let content = diffy_apply(&base, &parsed)
            .map_err(|err| patch_content_error(format!("修改补丁应用失败：{err}")))?;

        if !verify {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    patch_write_error(format!("创建目录失败：{}，{err}", parent.display()))
                })?;
            }
            fs::write(&target, content.as_bytes()).map_err(|err| {
                patch_write_error(format!("写入文件失败：{}，{err}", target.display()))
            })?;
            if source != target {
                fs::remove_file(&source).map_err(|err| {
                    patch_write_error(format!("删除原文件失败：{}，{err}", source.display()))
                })?;
            }
        }

        stats.updated += 1;
        if source != target {
            stats.moved += 1;
        }
        stats.files.push(json!({
            "action": if source == target { "update" } else { "move_update" },
            "source_rel": display_rel_path(&source),
            "source_abs": source.display().to_string(),
            "target_rel": display_rel_path(&target),
            "target_abs": target.display().to_string(),
        }));
    }

    Ok(stats)
}

fn split_unified_diff_sections(patch: &str) -> Result<Vec<String>> {
    let lines = patch.lines().collect::<Vec<_>>();
    if lines.len() < 3 {
        return Err(patch_parse_error("unified diff 内容过短，无法解析"));
    }

    let mut section_starts = Vec::new();
    for idx in 0..(lines.len() - 1) {
        if lines[idx].starts_with("--- ") && lines[idx + 1].starts_with("+++ ") {
            section_starts.push(idx);
        }
    }
    if section_starts.is_empty() {
        return Err(patch_parse_error("unified diff 缺少文件头（--- / +++）"));
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
        return Err(patch_parse_error("unified diff 文件路径为空"));
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
        return Err(patch_parse_error("unified diff 文件路径非法"));
    }
    Ok(path.to_string())
}

fn parse_bool_arg(raw: Option<&str>, default: bool) -> Result<bool> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(default);
    };
    match text.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(patch_parse_error(format!("布尔参数非法：{text}"))),
    }
}

fn patch_parse_error(message: impl Into<String>) -> anyhow::Error {
    anyhow!("[PATCH_PARSE] {}", message.into())
}

fn patch_path_error(message: impl Into<String>) -> anyhow::Error {
    anyhow!("[PATCH_PATH] {}", message.into())
}

fn patch_content_error(message: impl Into<String>) -> anyhow::Error {
    anyhow!("[PATCH_CONTENT] {}", message.into())
}

fn patch_write_error(message: impl Into<String>) -> anyhow::Error {
    anyhow!("[PATCH_WRITE] {}", message.into())
}
