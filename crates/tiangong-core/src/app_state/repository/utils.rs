use super::super::*;

pub(in crate::app_state) fn cleanup_empty_skill_install_dirs(
    removed_path: &Path,
    install_root: &Path,
) -> Result<()> {
    let mut current = removed_path.parent().map(Path::to_path_buf);
    while let Some(dir) = current {
        if dir == install_root || !dir.starts_with(install_root) {
            break;
        }
        let mut entries =
            fs::read_dir(&dir).with_context(|| format!("读取目录失败：{}", dir.display()))?;
        if entries.next().is_some() {
            break;
        }
        fs::remove_dir(&dir).with_context(|| format!("删除空目录失败：{}", dir.display()))?;
        current = dir.parent().map(Path::to_path_buf);
    }
    Ok(())
}

pub(in crate::app_state) fn converted_stage_cleanup_dir(
    install_path: &Path,
    converted: bool,
) -> Option<PathBuf> {
    if !converted {
        return None;
    }
    let imported_root = default_skills_storage_dir_path().join("imported");
    if install_path.starts_with(&imported_root) {
        Some(install_path.to_path_buf())
    } else {
        None
    }
}

pub(in crate::app_state) fn default_storage_root() -> PathBuf {
    user_storage_root()
}

pub(in crate::app_state) fn default_app_storage_path() -> PathBuf {
    default_storage_root().join("app.json")
}

pub(in crate::app_state) fn default_skills_config_path() -> PathBuf {
    default_storage_root().join("skills.json")
}

pub(in crate::app_state) fn default_mcp_config_path() -> PathBuf {
    default_storage_root().join("mcp.json")
}

pub(in crate::app_state) fn default_mcp_capability_cache_path() -> PathBuf {
    default_storage_root().join("mcp-tools-cache.json")
}

pub(in crate::app_state) fn default_sessions_dir_path() -> PathBuf {
    default_storage_root().join("sessions")
}

pub(in crate::app_state) fn default_legacy_storage_path() -> PathBuf {
    default_storage_root().join("sessions.json")
}

pub(in crate::app_state) fn default_skills_storage_dir_path() -> PathBuf {
    default_storage_root().join("skills")
}

pub(in crate::app_state) fn default_skills_lock_path() -> PathBuf {
    default_skills_storage_dir_path().join("skills-lock.json")
}

pub(in crate::app_state) fn default_mcp_lock_path() -> PathBuf {
    default_skills_storage_dir_path().join("mcp-lock.json")
}

fn user_storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

pub(in crate::app_state) fn user_home_dir() -> Option<PathBuf> {
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

pub(in crate::app_state) fn session_storage_path(sessions_dir: &Path, session_id: &str) -> PathBuf {
    sessions_dir.join(format!("{session_id}.json"))
}

pub(in crate::app_state) fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败：{}", parent.display()))?;
    }
    Ok(())
}

pub(in crate::app_state) fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("创建目录失败：{}", path.display()))
}

pub(in crate::app_state) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
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

pub(in crate::app_state) fn canonical_scru128_id(raw: &str) -> Option<String> {
    raw.trim()
        .parse::<scru128::Scru128Id>()
        .ok()
        .map(|id| id.to_string())
}

pub(in crate::app_state) fn new_scru128_string() -> String {
    scru128::new().to_string()
}

#[allow(dead_code)]
pub(in crate::app_state) fn elapsed_ms_u64(value: u128) -> u64 {
    value.min(u128::from(u64::MAX)) as u64
}

pub(in crate::app_state) fn normalize_model_list(
    models: Vec<String>,
    current_model: &str,
) -> Vec<String> {
    let mut list = Vec::new();
    let current = current_model.trim();
    if !current.is_empty() {
        list.push(current.to_string());
    }

    for model in models {
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        if list.iter().any(|item| item == model) {
            continue;
        }
        list.push(model.to_string());
    }
    list
}

pub(in crate::app_state) fn validate_agent_config(config: &AgentConfig) -> Result<()> {
    if config.skills.max_matches == 0 {
        return Err(anyhow!("skills.max_matches 必须大于 0"));
    }
    let mut seen_skill_ids = HashSet::new();
    for skill in &config.skills.installed {
        let id = skill.id.trim();
        if id.is_empty() {
            return Err(anyhow!("skills.installed 包含空 id"));
        }
        if !seen_skill_ids.insert(id.to_string()) {
            return Err(anyhow!("skills.installed 存在重复 id：{id}"));
        }
    }
    validate_mcp_config(&config.mcp)?;
    Ok(())
}

pub(in crate::app_state) fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(anyhow!("布尔值无效：{raw}（可用 true/false）")),
    }
}

pub(in crate::app_state) fn parse_list_value(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "[]" || trimmed == "-" {
        return Vec::new();
    }

    trimmed
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
}
