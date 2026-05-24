use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::model::{ModelClient, ModelRequest};
use crate::session::Message;
use crate::skill::SkillConversionArtifacts;

const MAX_TEXT_CHARS: usize = 12_000;
const MAX_FILE_LIST: usize = 32;

pub fn convert_external_skill_with_agent(
    client: &impl ModelClient,
    source_dir: &Path,
    need_skill_md: bool,
    need_skill_toml: bool,
) -> Result<SkillConversionArtifacts> {
    if !need_skill_md && !need_skill_toml {
        return Ok(SkillConversionArtifacts::default());
    }
    let canonical = fs::canonicalize(source_dir)
        .with_context(|| format!("解析 skill 目录失败：{}", source_dir.display()))?;
    if !canonical.is_dir() {
        return Err(anyhow!("skill 路径不是目录：{}", canonical.display()));
    }

    let prompt = build_convert_prompt(&canonical, need_skill_md, need_skill_toml)?;
    let request = ModelRequest {
        session_title: "skill-convert-agent".to_string(),
        user_input: prompt,
        context: Vec::<Message>::new(),
        thinking: Some(crate::model::ThinkingConfig {
            budget_tokens: 4096,
        }),
        reasoning_effort: None,
        thinking_disabled: false,
        include_media: false,
    };
    let response = client
        .complete(&request)
        .context("skill 转换智能体调用失败")?;
    let parsed = parse_skill_convert_output(&response.text).with_context(|| {
        let preview = response.text.chars().take(160).collect::<String>();
        format!("解析 skill 转换智能体输出失败，返回片段：{preview}")
    })?;

    Ok(SkillConversionArtifacts {
        skill_md: normalize_optional(parsed.skill_md),
        skill_toml: normalize_optional(parsed.skill_toml),
    })
}

fn build_convert_prompt(
    source_dir: &Path,
    need_skill_md: bool,
    need_skill_toml: bool,
) -> Result<String> {
    let fallback_id = source_dir
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("external-skill")
        .to_string();
    let files = list_top_level_entries(source_dir, MAX_FILE_LIST)?;
    let files_text = if files.is_empty() {
        "(empty)".to_string()
    } else {
        files.join("\n")
    };

    let existing_skill_md = read_optional_file(&source_dir.join("SKILL.md"), MAX_TEXT_CHARS);
    let existing_skill_toml = read_optional_file(&source_dir.join("skill.toml"), MAX_TEXT_CHARS);
    let fallback_doc = locate_external_markdown_entry(source_dir)
        .and_then(|path| read_optional_file(&path, MAX_TEXT_CHARS).map(|raw| (path, raw)));

    let fallback_doc_text = if let Some((path, raw)) = fallback_doc {
        format!(
            "文件：{}\n```markdown\n{}\n```",
            path.file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("unknown.md"),
            raw
        )
    } else {
        "(none)".to_string()
    };

    let existing_skill_md_text = existing_skill_md
        .map(|text| format!("```markdown\n{text}\n```"))
        .unwrap_or_else(|| "(none)".to_string());
    let existing_skill_toml_text = existing_skill_toml
        .map(|text| format!("```toml\n{text}\n```"))
        .unwrap_or_else(|| "(none)".to_string());

    Ok(format!(
        r#"你是天工的 skill 转换智能体，负责把外部 skill 目录转换成天工可安装格式。

任务目标：
1. 当 need_skill_md=true 时生成高质量 `SKILL.md` 内容。
2. 当 need_skill_toml=true 时生成合法 `skill.toml` 内容。
3. 当字段不需要生成时，输出空字符串。

输出要求：
1. 仅输出 JSON，不要输出解释，不要 markdown 包裹。
2. 严格使用以下结构：
{{
  "skill_md": "string",
  "skill_toml": "string"
}}
3. skill.toml 必须可解析，至少包含：
   - id/name/version/entry
   - [source] type/value
   - [requires] mcp = []
   - [permissions] fs_read/fs_write/cmd_exec/net（数组）
4. entry 固定为 `SKILL.md`，version 默认 `0.1.0`。
5. source.value 使用原始目录绝对路径：`{source_path}`。
6. id 优先基于目录名 `{fallback_id}` 生成（小写、短横线风格）。

输入上下文：
- source_path: {source_path}
- need_skill_md: {need_skill_md}
- need_skill_toml: {need_skill_toml}

顶层文件列表：
{files_text}

现有 SKILL.md：
{existing_skill_md_text}

现有 skill.toml：
{existing_skill_toml_text}

可参考外部文档：
{fallback_doc_text}"#,
        source_path = source_dir.display(),
        fallback_id = fallback_id,
        need_skill_md = need_skill_md,
        need_skill_toml = need_skill_toml,
        files_text = files_text,
        existing_skill_md_text = existing_skill_md_text,
        existing_skill_toml_text = existing_skill_toml_text,
        fallback_doc_text = fallback_doc_text,
    ))
}

#[derive(Debug, Deserialize)]
struct SkillConvertAgentOutput {
    #[serde(default)]
    skill_md: String,
    #[serde(default)]
    skill_toml: String,
}

fn parse_skill_convert_output(raw: &str) -> Result<SkillConvertAgentOutput> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("skill 转换智能体返回为空"));
    }
    for candidate in collect_json_candidates(raw) {
        if let Ok(parsed) = serde_json::from_str::<SkillConvertAgentOutput>(&candidate) {
            return Ok(parsed);
        }
    }
    Err(anyhow!("未匹配到有效 JSON"))
}

fn collect_json_candidates(raw: &str) -> Vec<String> {
    let mut candidates = vec![raw.to_string()];
    if let Some(stripped) = strip_markdown_code_block(raw) {
        candidates.push(stripped);
    }
    if let Some(extracted) = extract_first_json_object(raw) {
        candidates.push(extracted);
    }
    candidates.retain(|item| !item.trim().is_empty());
    candidates.dedup();
    candidates
}

fn strip_markdown_code_block(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("```") {
        return None;
    }
    let mut lines = trimmed.lines().collect::<Vec<_>>();
    if lines.len() < 2 {
        return None;
    }
    lines.remove(0);
    if lines
        .last()
        .is_some_and(|line| line.trim_start().starts_with("```"))
    {
        lines.pop();
    }
    Some(lines.join("\n").trim().to_string())
}

fn extract_first_json_object(raw: &str) -> Option<String> {
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in raw.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth = depth.saturating_add(1);
            }
            '}' => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    if let Some(begin) = start {
                        return Some(raw[begin..=idx].to_string());
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

fn read_optional_file(path: &Path, max_chars: usize) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    Some(truncate_chars(&raw, max_chars))
}

fn truncate_chars(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }
    let mut out = raw.chars().take(max_chars).collect::<String>();
    out.push_str("\n...(truncated)");
    out
}

fn list_top_level_entries(root: &Path, limit: usize) -> Result<Vec<String>> {
    let mut items = fs::read_dir(root)
        .with_context(|| format!("读取目录失败：{}", root.display()))?
        .flatten()
        .map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect::<Vec<_>>();
    items.sort();
    if items.len() > limit {
        items.truncate(limit);
    }
    Ok(items)
}

fn locate_external_markdown_entry(root: &Path) -> Option<PathBuf> {
    let preferred = [
        "README.md",
        "readme.md",
        "PROMPT.md",
        "prompt.md",
        "INSTRUCTIONS.md",
        "instructions.md",
    ];
    for name in preferred {
        let path = root.join(name);
        if path.is_file() {
            return Some(path);
        }
    }

    let mut markdown_files = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            if !path.is_file() {
                return false;
            }
            let is_markdown = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
            if !is_markdown {
                return false;
            }
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
        })
        .collect::<Vec<_>>();
    markdown_files.sort();
    markdown_files.into_iter().next()
}

fn normalize_optional(raw: String) -> Option<String> {
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}
