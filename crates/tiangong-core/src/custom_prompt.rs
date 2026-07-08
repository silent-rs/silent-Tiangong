//! 自定义 Prompt 独立存储（0.12.0+）。
//!
//! 将用户自定义 Prompt 从 `app.json` 的 `agent_config.custom_system_prompt`
//! 字段迁移到独立文件 `~/.tiangong/custom-prompt.md`，便于 CLI 编辑、
//! 用户备份与版本管理（避免 JSON 字符串转义问题）。
//!
//! # 加载优先级
//!
//! 1. `custom-prompt.md` 存在且非空 → 读取它（唯一事实来源）。
//! 2. 否则读取 `app.json` 中 `agent_config.custom_system_prompt`（兼容旧配置）。
//! 3. 再否则为空。
//!
//! # 写入行为
//!
//! `save` 写入 `custom-prompt.md` 后，调用方应清空 `app.json` 的旧字段，
//! 使 `custom-prompt.md` 成为唯一事实来源，消除歧义（见 RFC 0015）。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// 自定义 Prompt 独立文件路径：~/.tiangong/custom-prompt.md
pub fn custom_prompt_path() -> PathBuf {
    crate::storage::storage_root().join("custom-prompt.md")
}

/// 读取自定义 Prompt，优先 `custom-prompt.md`，回退 `legacy`（旧字段值）。
///
/// - `custom-prompt.md` 存在 → 读取其内容（去除首尾空白后非空才采用）。
/// - 否则返回 `legacy`（调用方传入的 app.json 旧字段值）。
pub fn load_custom_prompt(legacy: &str) -> Result<String> {
    load_custom_prompt_at(&custom_prompt_path(), legacy)
}

/// 从指定路径加载自定义 Prompt（供测试与自定义目录使用）。
pub fn load_custom_prompt_at(path: &Path, legacy: &str) -> Result<String> {
    if !path.exists() {
        return Ok(legacy.to_string());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("读取自定义 Prompt 失败：{}", path.display()))?;
    if content.trim().is_empty() {
        // 空文件视为未配置，回退旧字段
        Ok(legacy.to_string())
    } else {
        Ok(content)
    }
}

/// 保存自定义 Prompt 到 `custom-prompt.md`。
///
/// 调用方应在保存后清空 `app.json` 的 `custom_system_prompt` 旧字段。
pub fn save_custom_prompt(content: &str) -> Result<()> {
    save_custom_prompt_at(&custom_prompt_path(), content)
}

/// 保存自定义 Prompt 到指定路径（供测试使用）。
pub fn save_custom_prompt_at(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建配置目录失败：{}", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("写入自定义 Prompt 失败：{}", path.display()))
}

/// 删除 `custom-prompt.md`（清空 Prompt）。
///
/// 文件不存在视为成功。调用方应同时清空 `app.json` 的旧字段。
pub fn clear_custom_prompt() -> Result<()> {
    clear_custom_prompt_at(&custom_prompt_path())
}

/// 删除指定路径的自定义 Prompt 文件（供测试使用）。
pub fn clear_custom_prompt_at(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(path).with_context(|| format!("删除自定义 Prompt 失败：{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 为测试创建唯一的临时文件路径。
    fn temp_prompt_path(label: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("tiangong-cp-test-{nanos}-{label}"));
        dir.join("custom-prompt.md")
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn load_falls_back_to_legacy_when_no_file() {
        let path = temp_prompt_path("fallback");
        let result = load_custom_prompt_at(&path, "旧值").unwrap();
        assert_eq!(result, "旧值");
        cleanup(&path);
    }

    #[test]
    fn load_prefers_md_file_over_legacy() {
        let path = temp_prompt_path("prefer");
        save_custom_prompt_at(&path, "新的 Prompt").unwrap();
        let result = load_custom_prompt_at(&path, "旧值").unwrap();
        assert_eq!(result, "新的 Prompt");
        cleanup(&path);
    }

    #[test]
    fn empty_md_file_falls_back_to_legacy() {
        let path = temp_prompt_path("empty");
        save_custom_prompt_at(&path, "   \n  ").unwrap();
        let result = load_custom_prompt_at(&path, "旧值").unwrap();
        assert_eq!(result, "旧值");
        cleanup(&path);
    }

    #[test]
    fn clear_removes_file() {
        let path = temp_prompt_path("clear");
        save_custom_prompt_at(&path, "临时 Prompt").unwrap();
        assert!(path.exists());
        clear_custom_prompt_at(&path).unwrap();
        assert!(!path.exists());
        // 再次清空不报错
        clear_custom_prompt_at(&path).unwrap();
        cleanup(&path);
    }

    #[test]
    fn save_creates_parent_dir() {
        let path = temp_prompt_path("mkdir");
        assert!(!path.parent().unwrap().exists());
        save_custom_prompt_at(&path, "内容").unwrap();
        assert!(path.exists());
        cleanup(&path);
    }

    #[test]
    fn multiline_content_preserved() {
        let path = temp_prompt_path("multiline");
        let content = "第一行\n第二行\n\n第四行";
        save_custom_prompt_at(&path, content).unwrap();
        let result = load_custom_prompt_at(&path, "").unwrap();
        assert_eq!(result, content);
        cleanup(&path);
    }
}
