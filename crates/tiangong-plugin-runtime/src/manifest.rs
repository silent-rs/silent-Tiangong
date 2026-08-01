//! 已安装 WASM 插件的制品清单。

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::protocol::PROTOCOL_VERSION;

pub const MANIFEST_FILE: &str = "plugin.json";
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub wasm: WasmManifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<SidecarManifest>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WasmManifest {
    Detailed { binary: PathBuf },
    Legacy(PathBuf),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarManifest {
    /// 相对插件目录的可执行文件名，不包含平台可执行后缀。
    pub binary: PathBuf,
    #[serde(default = "default_transport_protocol")]
    pub transport_protocol: String,
    #[serde(default)]
    pub business_protocol: u32,
    #[serde(default = "default_startup_timeout_ms")]
    pub startup_timeout_ms: u64,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
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
        if self.version.trim().is_empty() {
            bail!("插件 {} 清单版本为空", self.id);
        }
        validate_relative_path(self.wasm_binary(), "wasm.binary")?;
        if let Some(sidecar) = &self.sidecar {
            validate_relative_path(&sidecar.binary, "sidecar.binary")?;
            if sidecar.transport_protocol.trim().is_empty() {
                bail!("插件 {} sidecar transport 版本为空", self.id);
            }
            if sidecar.startup_timeout_ms == 0 || sidecar.request_timeout_ms == 0 {
                bail!("插件 {} sidecar 超时时间必须大于 0", self.id);
            }
        }
        Ok(())
    }

    pub fn wasm_binary(&self) -> &Path {
        match &self.wasm {
            WasmManifest::Detailed { binary } | WasmManifest::Legacy(binary) => binary,
        }
    }

    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|item| item == permission)
    }
}

fn default_transport_protocol() -> String {
    PROTOCOL_VERSION.to_string()
}

const fn default_startup_timeout_ms() -> u64 {
    15_000
}

const fn default_request_timeout_ms() -> u64 {
    30_000
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
