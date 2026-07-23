//! bot 已安装版本记录——install/upgrade 后写入，检查更新时对比。

use serde::{Deserialize, Serialize};

use crate::BotId;
use crate::manifest::BotManifest;
use crate::paths;

/// 已安装制品的版本记录（持久化在 `~/.tiangong/bots/<id>/version.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVersion {
    /// 制品 id。
    pub artifact_id: String,
    /// 制品展示名称。
    #[serde(default)]
    pub name: String,
    /// 语义化版本。
    pub version: String,
    /// 安装时间。
    pub installed_at: String,
}

/// 读取已安装版本记录；文件不存在或解析失败返回 `None`。
pub fn read_installed_version(id: &BotId) -> Option<InstalledVersion> {
    let path = paths::bot_version_path(id);
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// 写入已安装版本记录（install/upgrade 成功后调用）。
pub fn write_installed_version(id: &BotId, manifest: &BotManifest) -> anyhow::Result<()> {
    let path = paths::bot_version_path(id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("创建版本记录目录失败：{} {e}", parent.display()))?;
    }
    let content = installed_version_json(manifest)?;
    std::fs::write(&path, content)
        .map_err(|e| anyhow::anyhow!("写入版本记录失败：{} {e}", path.display()))?;
    Ok(())
}

pub(crate) fn installed_version_json(manifest: &BotManifest) -> anyhow::Result<String> {
    let record = InstalledVersion {
        artifact_id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        installed_at: chrono::Local::now().naive_local().to_string(),
    };
    serde_json::to_string_pretty(&record).map_err(|e| anyhow::anyhow!("序列化版本记录失败：{e}"))
}

/// 对比线上版本与本地已安装版本。
///
/// 返回 `true` 表示线上有更新（本地不存在或线上版本更高）。
pub fn has_update(local: Option<&InstalledVersion>, remote_version: &str) -> bool {
    match local {
        None => true, // 本地无版本记录 → 视为需要安装
        Some(installed) => version_newer(remote_version, &installed.version),
    }
}

/// 判断 `a` 是否比 `b` 版本更高（语义化版本比较）。
fn version_newer(a: &str, b: &str) -> bool {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(va), Ok(vb)) => va > vb,
        // 解析失败时回退到字符串比较（不等则视为有更新）。
        _ => a != b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_newer_semver() {
        assert!(version_newer("0.2.0", "0.1.0"));
        assert!(version_newer("1.0.0", "0.9.9"));
        assert!(!version_newer("0.1.0", "0.1.0"));
        assert!(!version_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn version_newer_invalid_fallback() {
        assert!(version_newer("abc", "0.1.0"));
        assert!(!version_newer("0.1.0", "0.1.0"));
    }

    #[test]
    fn has_update_logic() {
        let installed = InstalledVersion {
            artifact_id: "feishu".into(),
            name: "飞书 Bot".into(),
            version: "0.1.0".into(),
            installed_at: "now".into(),
        };
        // 本地无记录 → 有更新。
        assert!(has_update(None, "0.1.0"));
        // 线上更高 → 有更新。
        assert!(has_update(Some(&installed), "0.2.0"));
        // 版本相同 → 无更新。
        assert!(!has_update(Some(&installed), "0.1.0"));
        // 线上更低 → 无更新。
        assert!(!has_update(Some(&installed), "0.0.9"));
    }
}
