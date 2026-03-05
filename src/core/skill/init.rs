use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

#[derive(Debug, Clone)]
pub struct SkillInitResult {
    pub dir: PathBuf,
    pub skill_id: String,
    pub skill_name: String,
}

pub fn init_tiangong_skill_scaffold(
    path: &Path,
    name: Option<&str>,
    id: Option<&str>,
    force: bool,
) -> Result<SkillInitResult> {
    let target_dir = resolve_target_dir(path)?;
    ensure_dir(&target_dir)?;

    let fallback = target_dir
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("new-skill");
    let requested_id = id.unwrap_or_default().trim().to_string();
    let skill_id = normalize_skill_id(&requested_id, fallback);
    if skill_id.is_empty() {
        return Err(anyhow!("skill id 不能为空"));
    }

    let requested_name = name.unwrap_or_default().trim().to_string();
    let skill_name = if requested_name.is_empty() {
        display_name_from_slug(&skill_id)
    } else {
        requested_name
    };

    let skill_md_path = target_dir.join("SKILL.md");
    let skill_toml_path = target_dir.join("skill.toml");
    if !force && (skill_md_path.exists() || skill_toml_path.exists()) {
        return Err(anyhow!(
            "目标目录已存在 SKILL.md 或 skill.toml，请使用 --force 覆盖：{}",
            target_dir.display()
        ));
    }

    let skill_md_content = build_initial_skill_markdown(&skill_name);
    fs::write(&skill_md_path, skill_md_content)
        .with_context(|| format!("写入 SKILL.md 失败：{}", skill_md_path.display()))?;
    let skill_toml_content = build_initial_skill_toml(&skill_id, &skill_name, &target_dir);
    fs::write(&skill_toml_path, skill_toml_content)
        .with_context(|| format!("写入 skill.toml 失败：{}", skill_toml_path.display()))?;

    Ok(SkillInitResult {
        dir: target_dir,
        skill_id,
        skill_name,
    })
}

fn resolve_target_dir(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(anyhow!("skill 目录不能为空"));
    }
    let raw = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("读取当前工作目录失败")?
            .join(path)
    };
    Ok(raw)
}

fn normalize_skill_id(raw: &str, fallback: &str) -> String {
    let source = if raw.trim().is_empty() {
        fallback
    } else {
        raw.trim()
    };
    let mut normalized = String::new();
    let mut previous_dash = false;
    for ch in source.chars() {
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() || lowered == '_' {
            normalized.push(lowered);
            previous_dash = false;
            continue;
        }
        if !previous_dash {
            normalized.push('-');
            previous_dash = true;
        }
    }
    normalized.trim_matches('-').to_string()
}

fn display_name_from_slug(raw: &str) -> String {
    let normalized = raw.trim().replace(['-', '_'], " ");
    let text = normalized.trim();
    if text.is_empty() {
        return "New Skill".to_string();
    }
    text.split_whitespace()
        .map(capitalize_ascii_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize_ascii_word(raw: &str) -> String {
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::new();
    out.push(first.to_ascii_uppercase());
    out.push_str(chars.as_str());
    out
}

fn build_initial_skill_markdown(skill_name: &str) -> String {
    format!(
        "# {skill_name}\n\n这是一个由 Tiangong 初始化生成的 Skill。\n\n## 适用场景\n- 在此补充该 skill 解决的问题。\n\n## 使用方式\n- 在此补充触发关键词与输入约定。\n\n## 约束\n- 仅执行与当前任务相关的操作。\n- 输出中尽量包含可复现的关键步骤。\n"
    )
}

fn build_initial_skill_toml(skill_id: &str, skill_name: &str, source_path: &Path) -> String {
    format!(
        "id = \"{}\"\nname = \"{}\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = []\n\n[permissions]\nfs_read = []\nfs_write = []\ncmd_exec = []\nnet = []\n",
        toml_escape(skill_id),
        toml_escape(skill_name),
        toml_escape(&source_path.display().to_string()),
    )
}

fn toml_escape(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("创建目录失败：{}", path.display()))
}
