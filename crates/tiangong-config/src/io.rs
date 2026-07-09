//! 配置磁盘 IO
//!
//! 所有 `~/.tiangong` 下的配置文件读写集中于此。core 不做任何配置磁盘 IO，
//! 由本模块加载后转换为 core 所需的纯数据配置注入。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use tiangong_llm::models_config::ModelsConfig;

const DEFAULT_CONTEXT_LIMIT: usize = 200_000;

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

/// 用户主目录（兼容 HOME / USERPROFILE / HOMEDRIVE+HOMEPATH）。
pub fn user_home_dir() -> Option<PathBuf> {
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

/// 天工存储根目录（`~/.tiangong`）。
pub fn storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

/// 自定义 Prompt 独立文件路径：`~/.tiangong/custom-prompt.md`。
pub fn custom_prompt_path() -> PathBuf {
    storage_root().join("custom-prompt.md")
}

// ---------------------------------------------------------------------------
// ModelsConfig
// ---------------------------------------------------------------------------

/// 从指定目录加载 models.json，文件不存在或解析失败返回空配置。
pub fn load_models_config_at(dir: &Path) -> ModelsConfig {
    let path = dir.join("models.json");
    if !path.exists() {
        return ModelsConfig::default();
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return ModelsConfig::default(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

/// 保存 ModelsConfig 到指定目录的 models.json。
///
/// 保存前自动将 routing 中未在 models 注册表里的条目补入 models，
/// 确保序列化时 routing 值能写为字符串引用（旧版本兼容）。
pub fn save_models_config_at(dir: &Path, config: &ModelsConfig) -> Result<()> {
    let mut cfg = config.clone();
    ensure_routing_models_registered(&mut cfg);

    let path = dir.join("models.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败：{}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(&cfg).with_context(|| "序列化 ModelsConfig 失败")?;
    std::fs::write(&path, content)
        .with_context(|| format!("写入 models.json 失败：{}", path.display()))?;
    Ok(())
}

/// 将 routing 中未在 models 注册表中的条目自动补入 models。
fn ensure_routing_models_registered(cfg: &mut ModelsConfig) {
    for entry in cfg.routing.values() {
        let exists = cfg
            .models
            .iter()
            .any(|(_, m)| m.provider == entry.provider && m.model == entry.model);
        if !exists {
            let key = format!("{}-{}", entry.provider, entry.model);
            if cfg.models.contains_key(&key) {
                continue;
            }
            cfg.models.insert(key, entry.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// 自定义 Prompt
// ---------------------------------------------------------------------------

/// 读取自定义 Prompt，优先 `custom-prompt.md`，回退 `legacy`（旧字段值）。
///
/// - 文件存在 → 读取其内容（去除首尾空白后非空才采用）。
/// - 否则返回 `legacy`。
pub fn load_custom_prompt_at(path: &Path, legacy: &str) -> Result<String> {
    if !path.exists() {
        return Ok(legacy.to_string());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("读取自定义 Prompt 失败：{}", path.display()))?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        Ok(legacy.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

/// 读取默认路径（`~/.tiangong/custom-prompt.md`）的自定义 Prompt。
pub fn load_custom_prompt(legacy: &str) -> Result<String> {
    load_custom_prompt_at(&custom_prompt_path(), legacy)
}

/// 保存自定义 Prompt 到指定路径（覆盖写，自动创建父目录）。
pub fn save_custom_prompt_at(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败：{}", parent.display()))?;
    }
    std::fs::write(path, content)
        .with_context(|| format!("写入自定义 Prompt 失败：{}", path.display()))?;
    Ok(())
}

/// 保存自定义 Prompt 到默认路径。
pub fn save_custom_prompt(content: &str) -> Result<()> {
    save_custom_prompt_at(&custom_prompt_path(), content)
}

/// 清除指定路径的自定义 Prompt（删除文件，文件不存在视为成功）。
pub fn clear_custom_prompt_at(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("删除自定义 Prompt 失败：{}", path.display()))?;
    }
    Ok(())
}

/// 清除默认路径的自定义 Prompt。
pub fn clear_custom_prompt() -> Result<()> {
    clear_custom_prompt_at(&custom_prompt_path())
}

// ---------------------------------------------------------------------------
// context_windows
// ---------------------------------------------------------------------------

/// 内嵌的默认 context_windows.json 内容（首次安装释放到用户目录）。
pub fn default_context_windows_json() -> &'static str {
    include_str!("resources/context_windows.json")
}

/// 首次安装：用户目录下不存在 context_windows.json 时释放内嵌默认内容。
pub fn ensure_context_windows(dir: &Path) {
    let path = dir.join("context_windows.json");
    if path.exists() {
        return;
    }
    let default_content = default_context_windows_json();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(err) = std::fs::write(&path, default_content) {
        tracing::warn!("写入 context_windows.json 失败：{err}");
    }
}

/// 根据模型名称从映射表解析 context_window。
///
/// 读取 `dir/context_windows.json`（不存在则用内嵌默认表），
/// 精确匹配 > 最长前缀匹配 > `DEFAULT_CONTEXT_LIMIT`。
pub fn resolve_context_limit_at(dir: &Path, model_name: &str) -> usize {
    const DEFAULT_MAP: &str = include_str!("resources/context_windows.json");

    let path = dir.join("context_windows.json");
    let content = if path.exists() {
        std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_MAP.to_string())
    } else {
        DEFAULT_MAP.to_string()
    };

    let map: std::collections::HashMap<String, Value> = match serde_json::from_str(&content) {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(
                "解析 context_windows.json 失败：{err}，使用默认值 {DEFAULT_CONTEXT_LIMIT}"
            );
            return DEFAULT_CONTEXT_LIMIT;
        }
    };

    // 精确匹配
    if let Some(Some(n)) = map.get(model_name).map(|v| v.as_u64()) {
        return n as usize;
    }

    // 前缀匹配：用最长的匹配前缀
    let mut best_match: Option<usize> = None;
    let mut best_len = 0;
    for (key, val) in &map {
        if key.starts_with('_') {
            continue;
        }
        if model_name.starts_with(key)
            && key.len() > best_len
            && let Some(n) = val.as_u64()
        {
            best_match = Some(n as usize);
            best_len = key.len();
        }
    }
    best_match.unwrap_or(DEFAULT_CONTEXT_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiangong_core::model::ProviderProtocol;
    use tiangong_llm::models_config::{
        ModelCapability, ModelEntry, ModelsConfig, ProviderConfig, RoutingSlot,
    };

    /// save_models_config_at 应把 routing 中未注册到 models 的条目自动补入 models。
    #[test]
    fn save_auto_registers_routing_entries_to_models() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = ModelsConfig::default();
        config.providers.insert(
            "p".to_string(),
            ProviderConfig {
                base_url: "https://api.test.com".to_string(),
                api_key: "k".to_string(),
                timeout_ms: 60_000,
                protocol: ProviderProtocol::OpenAiChatCompletions,
            },
        );
        config.routing.insert(
            RoutingSlot::Chat,
            ModelEntry {
                provider: "p".to_string(),
                model: "gpt-4".to_string(),
                capabilities: vec![ModelCapability::Chat],
                options: serde_json::json!({}),
            },
        );

        save_models_config_at(dir.path(), &config).unwrap();
        let reloaded = load_models_config_at(dir.path());
        assert!(reloaded.models.values().any(|m| m.model == "gpt-4"));
    }

    #[test]
    fn resolve_context_limit_exact_and_prefix_match() {
        let dir = tempfile::tempdir().unwrap();
        ensure_context_windows(dir.path());
        // 内嵌默认表含 gpt-4o 与 glm-4.5 前缀
        assert!(resolve_context_limit_at(dir.path(), "gpt-4o") > 0);
        assert!(resolve_context_limit_at(dir.path(), "glm-4.5-flash") > 0);
        // 未知模型回退默认值
        assert_eq!(
            resolve_context_limit_at(dir.path(), "totally-unknown-model"),
            DEFAULT_CONTEXT_LIMIT
        );
    }
}
