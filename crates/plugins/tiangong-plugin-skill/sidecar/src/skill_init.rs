//! Skill 脚手架生成（从原生插件 `skill_init.rs` 迁入）。
//!
//! 在指定目录生成 `skill.toml` 与 `SKILL.md`，纯文件写入，不涉及 registry 扫描。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use tiangong_plugin_skill_protocol::{InitSkillRequest, InitSkillResult};

/// 在目标目录生成 skill 脚手架。
pub fn init_skill_scaffold(req: InitSkillRequest) -> Result<InitSkillResult> {
    let path = Path::new(&req.path);
    let target_dir = resolve_target_dir(path)?;
    ensure_dir(&target_dir)?;

    let fallback = target_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("new-skill");
    let requested_id = req.id.as_deref().unwrap_or("").trim();
    let skill_id = normalize_skill_id(requested_id, fallback);
    if skill_id.is_empty() {
        return Err(anyhow!("skill id 不能为空"));
    }

    let requested_name = req.name.as_deref().unwrap_or("").trim();
    let display_name = if requested_name.is_empty() {
        display_name_from_slug(&skill_id)
    } else {
        requested_name.to_string()
    };

    // 检查文件是否已存在。
    let skill_md = target_dir.join("SKILL.md");
    let skill_toml = target_dir.join("skill.toml");
    if !req.force && (skill_md.exists() || skill_toml.exists()) {
        return Err(anyhow!(
            "目标目录已存在 SKILL.md 或 skill.toml，使用 --force 覆盖"
        ));
    }

    fs::write(&skill_md, build_initial_skill_markdown(&display_name))
        .with_context(|| format!("写入 SKILL.md 失败：{}", skill_md.display()))?;
    fs::write(
        &skill_toml,
        build_initial_skill_toml(&skill_id, &display_name),
    )
    .with_context(|| format!("写入 skill.toml 失败：{}", skill_toml.display()))?;

    Ok(InitSkillResult {
        dir: target_dir.display().to_string(),
        skill_id,
        skill_name: display_name,
    })
}

fn resolve_target_dir(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(anyhow!("目标路径不能为空"));
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = std::env::current_dir().context("获取当前目录失败")?;
        Ok(cwd.join(path))
    }
}

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("创建目录失败：{}", path.display()))
}

fn normalize_skill_id(raw: &str, fallback: &str) -> String {
    let source = if raw.is_empty() { fallback } else { raw };
    let mut result = String::new();
    let mut prev_dash = true;
    for ch in source.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == '_' {
            result.push(lower);
            prev_dash = false;
        } else if !prev_dash {
            result.push('-');
            prev_dash = true;
        }
    }
    result.trim_matches('-').to_string()
}

fn display_name_from_slug(raw: &str) -> String {
    if raw.is_empty() {
        return "New Skill".to_string();
    }
    let spaced = raw.replace(['-', '_'], " ");
    let capitalized: String = spaced
        .split_whitespace()
        .map(capitalize_ascii_word)
        .collect::<Vec<_>>()
        .join(" ");
    if capitalized.is_empty() {
        "New Skill".to_string()
    } else {
        capitalized
    }
}

fn capitalize_ascii_word(raw: &str) -> String {
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn build_initial_skill_markdown(name: &str) -> String {
    format!(
        r#"# {name}

## 适用场景

<!-- 描述这个 Skill 适用于什么任务 -->

## 使用方式

<!-- 描述如何调用这个 Skill（命令、脚本入口等） -->

## 约束

<!-- 描述这个 Skill 的限制与注意事项 -->
"#
    )
}

fn build_initial_skill_toml(skill_id: &str, skill_name: &str) -> String {
    format!(
        r#"id = "{id}"
name = "{name}"
version = "0.1.0"
entry = "SKILL.md"
available = true

[source]
kind = "local"
value = ""

[requires]

[permissions]
"#,
        id = toml_escape(skill_id),
        name = toml_escape(skill_name),
    )
}

fn toml_escape(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}
