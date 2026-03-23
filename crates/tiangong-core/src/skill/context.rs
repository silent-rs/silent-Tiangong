use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::agent_config::SkillsConfig;

use super::package::extract_skill_markdown_meta;
use super::util::format_skill_record;

#[derive(Debug, Clone)]
pub(super) struct SkillMeta {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

pub fn build_skill_hints(user_input: &str, config: &SkillsConfig) -> Vec<String> {
    if !config.enabled {
        return vec![format_skill_record("skipped", "all", "skills disabled")];
    }

    let (catalog, mut warnings) = scan_skills_with_warnings(config);
    let mut matched = match_skills(user_input, &catalog, config.max_matches);
    warnings.append(&mut matched);
    warnings
}

fn scan_skills_with_warnings(config: &SkillsConfig) -> (Vec<SkillMeta>, Vec<String>) {
    let mut roots = config
        .dirs
        .iter()
        .map(|dir| PathBuf::from(dir.trim()))
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();

    if roots.is_empty()
        && let Ok(codex_home) = std::env::var("CODEX_HOME")
    {
        roots.push(PathBuf::from(codex_home).join("skills"));
    }
    roots.push(default_installed_skills_scan_dir());

    let mut dedup = HashSet::new();
    roots.retain(|path| dedup.insert(path.display().to_string()));

    let mut warnings = Vec::new();
    let mut files = Vec::new();
    for root in roots {
        if !root.exists() {
            if root != default_installed_skills_scan_dir() {
                warnings.push(format_skill_record(
                    "error",
                    root.display(),
                    "skills dir not found",
                ));
            }
            continue;
        }
        collect_skill_files(&root, &mut files);
    }

    let mut catalog = files
        .into_iter()
        .filter_map(|path| parse_skill_file(&path))
        .collect::<Vec<_>>();
    let mut existing_names = catalog
        .iter()
        .map(|item| item.name.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    for installed in &config.installed {
        if !installed.enabled {
            continue;
        }
        let name = if installed.name.trim().is_empty() {
            installed.id.clone()
        } else {
            installed.name.clone()
        };
        if name.trim().is_empty() {
            continue;
        }
        let lowered = name.to_ascii_lowercase();
        if !existing_names.insert(lowered) {
            continue;
        }
        catalog.push(SkillMeta {
            name,
            description: if installed.description.trim().is_empty() {
                "已安装 skill".to_string()
            } else {
                installed.description.clone()
            },
            path: PathBuf::from(installed.source.value.trim()),
        });
    }
    (catalog, warnings)
}

fn match_skills(user_input: &str, catalog: &[SkillMeta], limit: usize) -> Vec<String> {
    if user_input.trim().is_empty() || catalog.is_empty() || limit == 0 {
        return Vec::new();
    }

    let input = user_input.to_ascii_lowercase();
    let mut scored = Vec::new();
    for skill in catalog {
        let mut score = 0usize;
        let name = skill.name.to_ascii_lowercase();
        if input.contains(&name) {
            score += 6;
        }

        for token in split_tokens(&input) {
            if token.len() < 2 {
                continue;
            }
            if name.contains(token) {
                score += 2;
            }
            if skill.description.to_ascii_lowercase().contains(token) {
                score += 1;
            }
        }

        if input.contains("浏览器")
            && (name.contains("playwright") || name.contains("chrome") || name.contains("browser"))
        {
            score += 3;
        }
        if input.contains("安装")
            && (name.contains("install") || name.contains("installer") || name.contains("skill"))
        {
            score += 2;
        }

        if score > 0 {
            scored.push((score, skill));
        }
    }

    scored.sort_by(|(a_score, a_skill), (b_score, b_skill)| {
        b_score
            .cmp(a_score)
            .then_with(|| a_skill.name.cmp(&b_skill.name))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, skill)| {
            let path = skill.path.display().to_string();
            format_skill_record("ok", &skill.name, &path)
        })
        .collect()
}

fn collect_skill_files(root: &Path, out: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
            {
                out.push(path);
            }
        }
    }
}

fn parse_skill_file(path: &Path) -> Option<SkillMeta> {
    let raw = fs::read_to_string(path).ok()?;
    let (name, description) = extract_skill_markdown_meta(&raw, path);

    Some(SkillMeta {
        name,
        description,
        path: path.to_path_buf(),
    })
}

fn split_tokens(input: &str) -> Vec<&str> {
    input
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .filter(|token| !token.is_empty())
        .collect()
}

fn default_installed_skills_scan_dir() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
        .join("skills")
        .join("installed")
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
