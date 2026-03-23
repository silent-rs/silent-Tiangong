use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::agent_config::{
    InstalledSkillConfig, SkillMcpRequirementConfig, SkillPermissionConfig, SkillSourceConfig,
};
use crate::session::now_text;

use super::analysis::{SkillConversionAnalysis, analyze_external_skill};

#[derive(Debug, Clone)]
pub struct PreparedSkillSource {
    pub install_path: PathBuf,
    pub converted: bool,
    pub conversion_notes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SkillConversionArtifacts {
    pub skill_md: Option<String>,
    pub skill_toml: Option<String>,
}

pub fn prepare_skill_source_for_install(
    path: &Path,
    convert_external: bool,
    llm_artifacts: Option<&SkillConversionArtifacts>,
    convert_env_values: &[(String, String)],
) -> Result<PreparedSkillSource> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("skill 目录不存在或不可访问：{}", path.display()))?;
    if !canonical.is_dir() {
        return Err(anyhow!("skill 路径不是目录：{}", canonical.display()));
    }
    if !convert_external {
        return Ok(PreparedSkillSource {
            install_path: canonical,
            converted: false,
            conversion_notes: Vec::new(),
        });
    }

    let analysis = analyze_external_skill(&canonical)?;

    let source_has_skill_md = canonical.join("SKILL.md").exists();
    let source_has_skill_toml = canonical.join("skill.toml").exists();
    if source_has_skill_md && source_has_skill_toml {
        return Ok(PreparedSkillSource {
            install_path: canonical,
            converted: false,
            conversion_notes: Vec::new(),
        });
    }

    let imported_root = user_storage_root().join("skills").join("imported");
    ensure_dir(&imported_root)?;

    let source_name = canonical
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("external-skill");
    let normalized_name = normalize_skill_id("", source_name);
    let stage_dir_name = if normalized_name.is_empty() {
        format!("external-skill-{}", scru128::new())
    } else {
        format!("{normalized_name}-{}", scru128::new())
    };
    let stage_dir = imported_root.join(stage_dir_name);
    copy_dir_recursive(&canonical, &stage_dir).with_context(|| {
        format!(
            "复制外部 skill 目录失败：{} -> {}",
            canonical.display(),
            stage_dir.display()
        )
    })?;

    let mut conversion_notes = Vec::new();
    if !analysis.dependencies.is_empty() {
        conversion_notes.push(format!("检测到依赖 {} 项", analysis.dependencies.len()));
    }
    if !analysis.env_vars.is_empty() {
        conversion_notes.push(format!("检测到环境变量 {} 项", analysis.env_vars.len()));
    }

    let stage_skill_md = stage_dir.join("SKILL.md");
    if !stage_skill_md.exists()
        && let Some(raw) = llm_artifacts.and_then(|item| item.skill_md.as_deref())
    {
        let generated = normalize_generated_skill_markdown(raw, &stage_dir);
        if !generated.trim().is_empty() {
            fs::write(&stage_skill_md, generated).with_context(|| {
                format!("写入模型转换 SKILL.md 失败：{}", stage_skill_md.display())
            })?;
            conversion_notes.push("使用模型辅助生成 SKILL.md".to_string());
        }
    }
    if !stage_skill_md.exists() {
        let fallback_md = locate_external_markdown_entry(&stage_dir)
            .ok_or_else(|| anyhow!("外部 skill 缺少可转换文档（README.md/PROMPT.md/*.md）"))?;
        let raw_fallback = fs::read_to_string(&fallback_md)
            .with_context(|| format!("读取外部 skill 文档失败：{}", fallback_md.display()))?;
        let generated = build_generated_skill_markdown(&raw_fallback, &stage_dir);
        fs::write(&stage_skill_md, generated)
            .with_context(|| format!("写入转换后的 SKILL.md 失败：{}", stage_skill_md.display()))?;
        let fallback_name = fallback_md
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("unknown.md");
        conversion_notes.push(format!("从 {fallback_name} 生成 SKILL.md"));
    }

    let stage_skill_toml = stage_dir.join("skill.toml");
    if !stage_skill_toml.exists()
        && let Some(raw) = llm_artifacts.and_then(|item| item.skill_toml.as_deref())
    {
        let generated = normalize_generated_skill_toml(raw);
        if !generated.trim().is_empty() {
            match toml::from_str::<SkillTomlManifest>(&generated) {
                Ok(_) => {
                    fs::write(&stage_skill_toml, generated).with_context(|| {
                        format!(
                            "写入模型转换 skill.toml 失败：{}",
                            stage_skill_toml.display()
                        )
                    })?;
                    conversion_notes.push("使用模型辅助生成 skill.toml".to_string());
                }
                Err(err) => {
                    conversion_notes.push(format!("模型生成 skill.toml 无效，回退规则生成：{err}"));
                }
            }
        }
    }
    if !stage_skill_toml.exists() {
        let raw_skill_md = fs::read_to_string(&stage_skill_md)
            .with_context(|| format!("读取 SKILL.md 失败：{}", stage_skill_md.display()))?;
        let (name, _) = extract_skill_markdown_meta(&raw_skill_md, &stage_skill_md);
        let fallback_id = stage_dir
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("external-skill");
        let mut generated_id = normalize_skill_id(&name, fallback_id);
        if generated_id.is_empty() {
            generated_id = format!("external-skill-{}", scru128::new());
        }
        let generated_toml = build_generated_skill_toml(&generated_id, &name, &canonical);
        fs::write(&stage_skill_toml, generated_toml).with_context(|| {
            format!(
                "写入转换后的 skill.toml 失败：{}",
                stage_skill_toml.display()
            )
        })?;
        conversion_notes.push("生成 skill.toml".to_string());
    }

    append_conversion_analysis_to_skill_md(
        &stage_skill_md,
        &analysis,
        convert_env_values,
        &mut conversion_notes,
    )?;
    write_env_helper_files(
        &stage_dir,
        &analysis,
        convert_env_values,
        &mut conversion_notes,
    )?;

    Ok(PreparedSkillSource {
        install_path: stage_dir,
        converted: true,
        conversion_notes,
    })
}

pub fn load_skill_from_local_dir(path: &Path) -> Result<InstalledSkillConfig> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("skill 目录不存在或不可访问：{}", path.display()))?;
    if !canonical.is_dir() {
        return Err(anyhow!("skill 路径不是目录：{}", canonical.display()));
    }

    let skill_md_path = canonical.join("SKILL.md");
    if !skill_md_path.exists() {
        return Err(anyhow!("skill 缺少 SKILL.md：{}", skill_md_path.display()));
    }
    let raw_skill_md = fs::read_to_string(&skill_md_path)
        .with_context(|| format!("读取 SKILL.md 失败：{}", skill_md_path.display()))?;
    let (md_name, md_description) = extract_skill_markdown_meta(&raw_skill_md, &skill_md_path);

    let skill_toml_path = canonical.join("skill.toml");
    let manifest = if skill_toml_path.exists() {
        let raw = fs::read_to_string(&skill_toml_path)
            .with_context(|| format!("读取 skill.toml 失败：{}", skill_toml_path.display()))?;
        let parsed: SkillTomlManifest = toml::from_str(&raw)
            .with_context(|| format!("解析 skill.toml 失败：{}", skill_toml_path.display()))?;
        Some(parsed)
    } else {
        None
    };

    let fallback_id = canonical
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("unknown-skill");
    let manifest_id = manifest
        .as_ref()
        .map(|item| item.id.trim().to_string())
        .unwrap_or_default();
    let skill_id = normalize_skill_id(&manifest_id, fallback_id);
    if skill_id.is_empty() {
        return Err(anyhow!("skill id 不能为空：{}", canonical.display()));
    }

    let skill_name = manifest
        .as_ref()
        .map(|item| item.name.trim().to_string())
        .filter(|item| !item.is_empty())
        .unwrap_or(md_name);
    let skill_version = manifest
        .as_ref()
        .map(|item| item.version.trim().to_string())
        .filter(|item| !item.is_empty())
        .unwrap_or_else(|| "0.1.0".to_string());
    let skill_entry = manifest
        .as_ref()
        .map(|item| item.entry.trim().to_string())
        .filter(|item| !item.is_empty())
        .unwrap_or_else(|| "SKILL.md".to_string());

    let source = manifest
        .as_ref()
        .map(|item| SkillSourceConfig {
            kind: if item.source.r#type.trim().is_empty() {
                "local".to_string()
            } else {
                item.source.r#type.trim().to_string()
            },
            value: if item.source.value.trim().is_empty() {
                canonical.display().to_string()
            } else {
                item.source.value.trim().to_string()
            },
        })
        .unwrap_or_else(|| SkillSourceConfig {
            kind: "local".to_string(),
            value: canonical.display().to_string(),
        });

    let requires_mcp = manifest
        .as_ref()
        .map(|item| item.requires.mcp.clone())
        .unwrap_or_default();
    let permissions = manifest
        .as_ref()
        .map(|item| item.permissions.clone())
        .unwrap_or_default();

    let managed_mcp_servers = requires_mcp
        .iter()
        .filter_map(|item| {
            let mcp_id = item.id.trim();
            if mcp_id.is_empty() {
                None
            } else {
                Some(format!("skill::{skill_id}::{mcp_id}"))
            }
        })
        .collect::<Vec<_>>();

    Ok(InstalledSkillConfig {
        id: skill_id,
        name: skill_name,
        version: skill_version,
        description: md_description,
        entry: skill_entry,
        enabled: true,
        installed_at: now_text(),
        managed_mcp_servers,
        source,
        requires_mcp,
        permissions,
    })
}

pub(super) fn extract_skill_markdown_meta(raw: &str, path: &Path) -> (String, String) {
    let mut name = path
        .parent()
        .and_then(|v| v.file_name())
        .and_then(|v| v.to_str())
        .unwrap_or("unknown-skill")
        .to_string();
    let mut description = "技能描述缺失".to_string();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("# ") {
            name = value.trim().to_string();
            continue;
        }
        if !trimmed.starts_with('#') {
            description = trimmed.to_string();
            break;
        }
    }

    (name, description)
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

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillTomlManifest {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    entry: String,
    #[serde(default)]
    source: SkillTomlSource,
    #[serde(default)]
    requires: SkillTomlRequires,
    #[serde(default)]
    permissions: SkillPermissionConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillTomlSource {
    #[serde(default, rename = "type")]
    r#type: String,
    #[serde(default)]
    value: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillTomlRequires {
    #[serde(default)]
    mcp: Vec<SkillMcpRequirementConfig>,
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

fn build_generated_skill_markdown(raw: &str, root: &Path) -> String {
    let fallback_name = root
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("External Skill");
    let has_title = raw.lines().any(|line| line.trim_start().starts_with("# "));
    if has_title {
        return raw.to_string();
    }
    let body = raw.trim();
    if body.is_empty() {
        format!("# {fallback_name}\n\n该 Skill 由 Tiangong 自动转换生成。\n")
    } else {
        format!("# {fallback_name}\n\n{body}\n")
    }
}

fn append_conversion_analysis_to_skill_md(
    skill_md_path: &Path,
    analysis: &SkillConversionAnalysis,
    convert_env_values: &[(String, String)],
    conversion_notes: &mut Vec<String>,
) -> Result<()> {
    if analysis.dependencies.is_empty() && analysis.env_vars.is_empty() {
        return Ok(());
    }
    let raw = fs::read_to_string(skill_md_path)
        .with_context(|| format!("读取 SKILL.md 失败：{}", skill_md_path.display()))?;
    let marker = "## 天工转换分析";
    if raw.contains(marker) {
        return Ok(());
    }

    let mut section = String::new();
    section.push_str("\n\n");
    section.push_str(marker);
    section.push_str("\n\n");
    if !analysis.dependencies.is_empty() {
        section.push_str("### 检测到的依赖\n");
        for dep in &analysis.dependencies {
            section.push_str("- `");
            section.push_str(dep);
            section.push_str("`\n");
        }
        section.push('\n');
    }
    if !analysis.env_vars.is_empty() {
        section.push_str("### 检测到的环境变量\n");
        for key in &analysis.env_vars {
            section.push_str("- `");
            section.push_str(key);
            section.push_str("`\n");
        }
        section.push('\n');
    }
    if !convert_env_values.is_empty() {
        section.push_str("### 安装时已提供环境变量\n");
        for (key, _value) in convert_env_values {
            section.push_str("- `");
            section.push_str(key);
            section.push_str("`\n");
        }
        section.push('\n');
    }
    section.push_str("以上信息由转换器自动分析，仅供安装时排查与补全配置。\n");

    let merged = format!("{}{}", raw.trim_end(), section);
    fs::write(skill_md_path, merged)
        .with_context(|| format!("写入转换增强 SKILL.md 失败：{}", skill_md_path.display()))?;
    conversion_notes.push("已将依赖与环境变量分析写入 SKILL.md".to_string());
    Ok(())
}

fn write_env_helper_files(
    stage_dir: &Path,
    analysis: &SkillConversionAnalysis,
    convert_env_values: &[(String, String)],
    conversion_notes: &mut Vec<String>,
) -> Result<()> {
    if !analysis.env_vars.is_empty() {
        let env_example = stage_dir.join(".env.example");
        if !env_example.exists() {
            let content = analysis
                .env_vars
                .iter()
                .map(|key| format!("{key}="))
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(&env_example, format!("{content}\n"))
                .with_context(|| format!("写入 .env.example 失败：{}", env_example.display()))?;
            conversion_notes.push("生成 .env.example".to_string());
        }
    }

    if convert_env_values.is_empty() {
        return Ok(());
    }
    let env_local = stage_dir.join(".env.local");
    let content = convert_env_values
        .iter()
        .filter_map(|(key, value)| {
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                None
            } else {
                Some(format!("{key}={}", value.replace('\n', "\\n")))
            }
        })
        .collect::<Vec<_>>();
    if content.is_empty() {
        return Ok(());
    }
    fs::write(&env_local, format!("{}\n", content.join("\n")))
        .with_context(|| format!("写入 .env.local 失败：{}", env_local.display()))?;
    conversion_notes.push(format!("写入 .env.local（{} 项）", content.len()));
    Ok(())
}

fn normalize_generated_skill_markdown(raw: &str, root: &Path) -> String {
    let cleaned = strip_markdown_code_fence(raw).trim().to_string();
    if cleaned.is_empty() {
        return String::new();
    }
    build_generated_skill_markdown(&cleaned, root)
}

fn build_generated_skill_toml(skill_id: &str, skill_name: &str, source_path: &Path) -> String {
    let name = if skill_name.trim().is_empty() {
        skill_id
    } else {
        skill_name.trim()
    };
    format!(
        "id = \"{}\"\nname = \"{}\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\n\n[source]\ntype = \"external\"\nvalue = \"{}\"\n\n[requires]\nmcp = []\n\n[permissions]\nfs_read = []\nfs_write = []\ncmd_exec = []\nnet = []\n",
        toml_escape(skill_id),
        toml_escape(name),
        toml_escape(&source_path.display().to_string()),
    )
}

fn normalize_generated_skill_toml(raw: &str) -> String {
    strip_markdown_code_fence(raw).trim().to_string()
}

fn strip_markdown_code_fence(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let mut lines = trimmed.lines().collect::<Vec<_>>();
    if lines.len() < 2 {
        return trimmed.to_string();
    }
    lines.remove(0);
    if lines
        .last()
        .is_some_and(|line| line.trim_start().starts_with("```"))
    {
        lines.pop();
    }
    lines.join("\n")
}

fn toml_escape(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

fn user_storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buf = PathBuf::from(drive);
            buf.push(path);
            Some(buf)
        }
        _ => None,
    }
}

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("创建目录失败：{}", path.display()))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        return Err(anyhow!("复制目录失败，源不是目录：{}", src.display()));
    }
    ensure_dir(dst)?;
    for entry in fs::read_dir(src).with_context(|| format!("读取目录失败：{}", src.display()))?
    {
        let entry = entry.with_context(|| format!("读取目录项失败：{}", src.display()))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("读取目录项类型失败：{}", src_path.display()))?;
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
            continue;
        }
        if file_type.is_file() {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "复制文件失败：{} -> {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}
