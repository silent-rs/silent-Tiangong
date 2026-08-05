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
    /// 插件适用的运行入口。未声明则全部入口可用（向后兼容）。
    ///
    /// 合法值：`desktop`、`cli`、`server`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoints: Option<Vec<String>>,
    /// 插件依赖的模型能力。未声明则不需要模型（向后兼容）。
    ///
    /// runtime 据此判断对应能力是否已配置端点；未配置时插件保持已安装但不注册工具。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_requirements: Option<Vec<ModelRequirement>>,
    /// 插件是否需要访问天工存储根目录（~/.tiangong）。
    ///
    /// 为 true 时，runtime 在 WASI 上下文中额外 preopen storage_root 目录，
    /// WASM 组件可直接读写其中的文件（如 custom-prompt.md）。
    /// 默认 false，向后兼容。
    #[serde(default)]
    pub storage_access: bool,
}

/// 单项模型能力需求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequirement {
    /// 能力标识，对齐 `ModelCapability` 的 snake_case：`multimodal`、`image_generation`、
    /// `video_generation`、`tts`、`stt`、`chat`、`embedding`、`rerank`。
    pub kind: String,
    /// 是否必需：`true` 时对应能力未配置则插件不注册工具；`false` 时仅记录告警。
    #[serde(default = "default_required")]
    pub required: bool,
}

const fn default_required() -> bool {
    true
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
        if self.permissions.iter().any(|item| item.trim().is_empty()) {
            bail!("插件 {} permissions 不能包含空值", self.id);
        }
        let unique_permissions = self
            .permissions
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if unique_permissions.len() != self.permissions.len() {
            bail!("插件 {} permissions 不能包含重复值", self.id);
        }
        if self.sidecar.is_some() && !self.has_permission("sidecar.invoke") {
            bail!(
                "插件 {} 声明 sidecar 时必须声明 sidecar.invoke 权限",
                self.id
            );
        }
        // 校验入口声明
        if let Some(entrypoints) = &self.entrypoints {
            for ep in entrypoints {
                if !matches!(ep.as_str(), "desktop" | "cli" | "server") {
                    bail!(
                        "插件 {} entrypoints 含非法值 {ep}（仅允许 desktop/cli/server）",
                        self.id
                    );
                }
            }
        }
        // 校验模型能力声明
        if let Some(requirements) = &self.model_requirements {
            for req in requirements {
                if !matches!(
                    req.kind.as_str(),
                    "chat"
                        | "multimodal"
                        | "image_generation"
                        | "video_generation"
                        | "tts"
                        | "stt"
                        | "embedding"
                        | "rerank"
                ) {
                    bail!(
                        "插件 {} model_requirements 含非法能力类型 {}（对齐 ModelCapability snake_case）",
                        self.id,
                        req.kind
                    );
                }
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

    /// 插件是否在指定入口可用。未声明 entrypoints 则全部可用（向后兼容）。
    pub fn available_at(&self, entrypoint: &str) -> bool {
        match &self.entrypoints {
            Some(entrypoints) => entrypoints.iter().any(|ep| ep == entrypoint),
            None => true,
        }
    }

    /// 返回必需的模型能力列表（required=true 的 kind）。
    pub fn required_model_capabilities(&self) -> Vec<&str> {
        self.model_requirements
            .as_ref()
            .map(|reqs| {
                reqs.iter()
                    .filter(|r| r.required)
                    .map(|r| r.kind.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 返回缺失的必需能力（传入的已配置能力列表之外的必需项）。
    pub fn missing_capabilities<'a>(&'a self, configured: &'a [&str]) -> Vec<&'a str> {
        self.required_model_capabilities()
            .into_iter()
            .filter(|cap| !configured.contains(cap))
            .collect()
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
