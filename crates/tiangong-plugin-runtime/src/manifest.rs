//! 已安装 WASM 插件的制品清单。

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const MANIFEST_FILE: &str = "plugin.json";
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub wasm: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<SidecarManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarManifest {
    /// 相对插件目录的可执行文件名，不包含平台可执行后缀。
    pub binary: PathBuf,
    /// 相对 storage root 的 endpoint 文件。
    pub endpoint: PathBuf,
    /// 相对 storage root 的日志文件。
    pub log: PathBuf,
}

impl PluginManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("读取插件清单失败: {}", path.display()))?;
        let manifest: Self = serde_json::from_str(&content)
            .with_context(|| format!("解析插件清单失败: {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            bail!(
                "插件 {} 清单版本不支持: expected={MANIFEST_SCHEMA_VERSION}, actual={}",
                self.id,
                self.schema_version
            );
        }
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            bail!("插件清单包含无效 ID: {}", self.id);
        }
        validate_relative_path(&self.wasm, "wasm")?;
        if let Some(sidecar) = &self.sidecar {
            validate_relative_path(&sidecar.binary, "sidecar.binary")?;
            validate_relative_path(&sidecar.endpoint, "sidecar.endpoint")?;
            validate_relative_path(&sidecar.log, "sidecar.log")?;
        }
        Ok(())
    }
}

fn validate_relative_path(path: &Path, field: &str) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("插件清单字段 {field} 必须是安全的相对路径");
    }
    Ok(())
}
