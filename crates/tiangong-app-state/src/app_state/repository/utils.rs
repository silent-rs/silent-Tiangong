use super::super::*;

/// 用户主目录（兼容 HOME / USERPROFILE / HOMEDRIVE+HOMEPATH）。
///
/// 路径计算归 app 层所有；core 的 `storage` 模块不做环境变量计算，
/// 由 app 层解析后注入。
pub(crate) fn user_home_dir() -> Option<PathBuf> {
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

/// 天工存储根目录（`~/.tiangong`），由 app 层统一计算。
///
/// 主目录不可用时回退到当前目录。这是 storage_root 的**唯一对外来源**——
/// 外部（plugin / entry / tauri）取存储根目录都应走本函数，不应依赖 core 注入态。
pub fn storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

/// 把解析好的存储根目录注入 core，作为 core 内所有持久化点的唯一来源。
///
/// 在 `TiangongState::load_or_default` 启动时调用。可重复 set（供单测隔离）。
pub(crate) fn init_storage_root() {
    tiangong_core::storage::set_storage_root(storage_root());
}

pub(in crate::app_state) fn default_storage_root() -> PathBuf {
    storage_root()
}

pub(in crate::app_state) fn default_app_storage_path() -> PathBuf {
    default_storage_root().join("app.json")
}

pub(in crate::app_state) fn default_workspace_dir() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

pub(in crate::app_state) fn default_sessions_dir_path() -> PathBuf {
    default_storage_root().join("sessions")
}

pub(in crate::app_state) fn default_legacy_storage_path() -> PathBuf {
    default_storage_root().join("sessions.json")
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

/// 校验 AgentConfig（core 维护的 agent runtime 配置）。
///
/// 稳定契约（请勿依赖此方法做更多校验）：
/// - 当前无可失败项——`trust_mode` / `custom_system_prompt` / `reasoning_effort`
///   均有合法默认值，反序列化后始终有效，故恒返回 `Ok(())`；
/// - 扩展能力配置（外部工具、技能等）不在此校验，由各 plugin 在自己的管理
///   方法内提供校验逻辑；
/// - 保留此入口供 `agent_config_facade` 调用点兼容与未来扩展（如新增可失败
///   的 runtime 字段时在此补充）。
pub fn validate_agent_config(_config: &AgentConfig) -> Result<()> {
    Ok(())
}

#[allow(dead_code)]
pub(in crate::app_state) fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(anyhow!("布尔值无效：{raw}（可用 true/false）")),
    }
}
