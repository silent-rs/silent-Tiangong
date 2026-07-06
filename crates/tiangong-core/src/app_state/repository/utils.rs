use super::super::*;

pub(in crate::app_state) fn default_storage_root() -> PathBuf {
    user_storage_root()
}

pub(in crate::app_state) fn default_app_storage_path() -> PathBuf {
    default_storage_root().join("app.json")
}

pub(in crate::app_state) fn default_workspace_dir() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
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

pub fn default_skills_storage_dir_path() -> PathBuf {
    default_storage_root().join("skills")
}

pub fn default_mcp_lock_path() -> PathBuf {
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

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("创建目录失败：{}", path.display()))
}

pub(in crate::app_state) fn canonical_scru128_id(raw: &str) -> Option<String> {
    raw.trim()
        .parse::<scru128::Id>()
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

pub fn validate_agent_config(config: &AgentConfig) -> Result<()> {
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
