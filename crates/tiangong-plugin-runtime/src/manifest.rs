//! 已安装 WASM 插件的制品清单。

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::protocol::PROTOCOL_VERSION;
use crate::slots::{OPEN_MODE_SLOT, OpenMode, SandboxKind, SlotRegistry, UiContribution};

pub const MANIFEST_FILE: &str = "plugin.json";
/// schema v1：现有清单，无 `ui`/`capabilities`，设置页贡献由 WASM 运行时声明。
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
/// schema v2：新增 `capabilities` 与 `ui.contributions`（Slot/沙箱/打开模式）。
pub const MANIFEST_SCHEMA_VERSION_V2: u32 = 2;

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
    #[serde(default)]
    pub storage_access: bool,
    /// 能力声明（schema v2 新增）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<CapabilitiesManifest>,
    /// UI 贡献声明（schema v2 新增）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<UiManifest>,
}

/// 能力声明（schema v2 的 `capabilities` 字段）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesManifest {
    /// 实现工具接缝（WASM 内 tool-specs/handle-tool）。
    #[serde(default)]
    pub tools: bool,
    /// 实现提示词接缝。
    #[serde(default)]
    pub prompt: bool,
    /// 实现生命周期接缝。
    #[serde(default)]
    pub lifecycle: bool,
    /// 参与审批接缝。
    #[serde(default)]
    pub approval: bool,
    /// 处理交互接缝（表单/选择/填写）。
    #[serde(default)]
    pub interaction: bool,
    /// 订阅的事件命名空间（如 `session.*`、`tool.*`）。
    #[serde(default)]
    pub events: Vec<String>,
}

/// UI 贡献声明（schema v2 的 `ui` 字段）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiManifest {
    /// 全部贡献的默认沙箱级别，缺省 `shadow`；贡献级声明优先。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxKind>,
    #[serde(default)]
    pub contributions: Vec<UiContributionDecl>,
}

/// manifest 中声明的单个 UI 贡献（`ui.contributions[]`）。
///
/// `sandbox`/`open_mode` 保留「未声明」语义以便校验，归一化结果见
/// [`PluginManifest::ui_contributions`]。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiContributionDecl {
    /// 目标挂载点，必须是 Slot Registry 登记的合法 ID。
    pub slot: String,
    /// 贡献 ID，插件内唯一。
    pub id: String,
    /// 展示标题，缺省用 `id`。
    #[serde(default)]
    pub title: String,
    /// 用途说明（矩阵卡片等展示位）。
    #[serde(default)]
    pub description: String,
    /// 图标名或内联 SVG。
    #[serde(default)]
    pub icon: String,
    /// 入口 HTML（相对插件目录）。
    pub entry: String,
    /// 打开模式，仅对 `extension.tab` 生效，缺省 `singleton`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_mode: Option<OpenMode>,
    /// 需要注入的上下文键，必须是目标 Slot 声明支持的键。
    #[serde(default)]
    pub context: Vec<String>,
    /// 沙箱级别，缺省落到 `ui.sandbox`，再缺省 `shadow`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxKind>,
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
        if !matches!(
            self.schema_version,
            MANIFEST_SCHEMA_VERSION | MANIFEST_SCHEMA_VERSION_V2
        ) {
            bail!(
                "插件 {} 清单版本不支持: expected={MANIFEST_SCHEMA_VERSION}|{MANIFEST_SCHEMA_VERSION_V2}, actual={}",
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
        let unique_permissions = self.permissions.iter().collect::<BTreeSet<_>>();
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
        // v1 清单不允许 v2 字段；v2 清单校验新增字段
        if self.schema_version == MANIFEST_SCHEMA_VERSION {
            if self.capabilities.is_some() {
                bail!(
                    "插件 {} 使用 schema_version 1 但声明了 capabilities 字段，请升级 schema_version 为 {MANIFEST_SCHEMA_VERSION_V2}",
                    self.id
                );
            }
            if self.ui.is_some() {
                bail!(
                    "插件 {} 使用 schema_version 1 但声明了 ui 字段，请升级 schema_version 为 {MANIFEST_SCHEMA_VERSION_V2}",
                    self.id
                );
            }
        } else {
            self.validate_v2()?;
        }
        Ok(())
    }

    /// schema v2 新增字段校验：capabilities 事件命名空间与 ui.contributions。
    fn validate_v2(&self) -> Result<()> {
        if let Some(capabilities) = &self.capabilities {
            for namespace in &capabilities.events {
                if namespace.trim().is_empty() || !namespace.contains('.') {
                    bail!(
                        "插件 {} capabilities.events 含非法命名空间 {namespace}（应为点分层级如 session.*）",
                        self.id
                    );
                }
            }
        }

        let Some(ui) = &self.ui else {
            return Ok(());
        };
        let registry = SlotRegistry::builtin();
        let mut seen_ids = BTreeSet::new();
        for decl in &ui.contributions {
            if decl.id.trim().is_empty() {
                bail!("插件 {} ui.contributions 包含空 id", self.id);
            }
            if !seen_ids.insert(decl.id.as_str()) {
                bail!("插件 {} ui.contributions 的 id {} 重复", self.id, decl.id);
            }
            if decl.entry.trim().is_empty() {
                bail!("插件 {} 贡献 {} 缺少 entry（入口 HTML）", self.id, decl.id);
            }
            validate_relative_path(
                Path::new(&decl.entry),
                &format!("ui.contributions[{}].entry", decl.id),
            )?;
            let slot = registry
                .validate(&decl.slot)
                .with_context(|| format!("插件 {} 贡献 {} 的 slot 无效", self.id, decl.id))?;
            if decl.open_mode.is_some() && decl.slot != OPEN_MODE_SLOT {
                bail!(
                    "插件 {} 贡献 {} 的 open_mode 仅对 {OPEN_MODE_SLOT} 生效，{} 不支持",
                    self.id,
                    decl.id,
                    decl.slot
                );
            }
            for key in &decl.context {
                let supported = slot.context.iter().any(|ctx| ctx.as_str() == key);
                if !supported {
                    bail!(
                        "插件 {} 贡献 {} 声明的上下文 {key} 不被 slot {} 支持（允许：{}）",
                        self.id,
                        decl.id,
                        decl.slot,
                        slot.context
                            .iter()
                            .map(|ctx| ctx.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
        }
        Ok(())
    }

    /// 归一化后的 UI 贡献列表（schema v2）。
    ///
    /// 沙箱级别逐级取值：贡献级 `sandbox` → `ui.sandbox` → `shadow`；
    /// 打开模式缺省 `singleton`。v1 清单返回空（设置页贡献由 WASM 运行时声明）。
    pub fn ui_contributions(&self) -> Vec<UiContribution> {
        let Some(ui) = &self.ui else {
            return Vec::new();
        };
        let default_sandbox = ui.sandbox.unwrap_or_default();
        ui.contributions
            .iter()
            .map(|decl| UiContribution {
                slot: decl.slot.clone(),
                title: if decl.title.trim().is_empty() {
                    decl.id.clone()
                } else {
                    decl.title.clone()
                },
                description: decl.description.clone(),
                id: decl.id.clone(),
                icon: decl.icon.clone(),
                entry: decl.entry.clone(),
                open_mode: decl.open_mode.unwrap_or_default(),
                context: decl.context.clone(),
                sandbox: decl.sandbox.unwrap_or(default_sandbox),
            })
            .collect()
    }

    /// v2 UI 贡献的 `native` 沙箱仅对携带有效官方签名的插件开放。
    ///
    /// 签名验证（`verify_signed_release`）完成后调用；结构校验见 [`Self::validate`]。
    pub fn validate_ui_native_sandbox(&self, official_signed: bool) -> Result<()> {
        if official_signed {
            return Ok(());
        }
        for contribution in self.ui_contributions() {
            if contribution.sandbox == SandboxKind::Native {
                bail!(
                    "插件 {} 的 UI 贡献 {} 声明 native 沙箱，仅官方签名插件可用",
                    self.id,
                    contribution.id
                );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// v1 基线清单（无 ui/capabilities），等价旧规则。
    fn v1_json() -> String {
        r#"{
            "schema_version": 1,
            "id": "com.example.legacy",
            "version": "1.0.0",
            "wasm": { "binary": "plugin.wasm" },
            "permissions": []
        }"#
        .to_string()
    }

    /// v2 完整清单：capabilities + ui.contributions（extension.tab 多实例）。
    fn v2_json() -> String {
        r#"{
            "schema_version": 2,
            "id": "com.example.board",
            "version": "1.0.0",
            "wasm": { "binary": "plugin.wasm" },
            "permissions": ["bridge.call"],
            "capabilities": {
                "tools": true,
                "prompt": true,
                "lifecycle": true,
                "approval": false,
                "interaction": true,
                "events": ["session.*", "tool.*"]
            },
            "ui": {
                "sandbox": "shadow",
                "contributions": [
                    {
                        "slot": "extension.tab",
                        "id": "board-tab",
                        "title": "看板",
                        "icon": "board",
                        "entry": "index.html",
                        "open_mode": "multi",
                        "context": ["session", "workspace"]
                    },
                    {
                        "slot": "settings.plugin-page",
                        "id": "board-settings",
                        "entry": "settings.html"
                    }
                ]
            }
        }"#
        .to_string()
    }

    fn parse(json: &str) -> Result<PluginManifest> {
        let manifest: PluginManifest = serde_json::from_str(json)?;
        manifest.validate()?;
        Ok(manifest)
    }

    #[test]
    fn v1_清单按旧规则解析() {
        let manifest = parse(&v1_json()).expect("v1 应兼容解析");
        assert!(manifest.capabilities.is_none());
        assert!(manifest.ui.is_none());
        assert!(manifest.ui_contributions().is_empty());
    }

    #[test]
    fn v1_清单携带_v2_字段被拒绝() {
        for field in ["capabilities", "ui"] {
            let json = v1_json().replace(
                "\"permissions\": []",
                &format!("\"permissions\": [], \"{field}\": {{}}"),
            );
            let error = parse(&json).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains(field), "应指出非法字段 {field}: {message}");
            assert!(message.contains("schema_version 为 2"));
        }
    }

    #[test]
    fn v2_清单解析出完整贡献() {
        let manifest = parse(&v2_json()).expect("v2 应解析通过");
        let capabilities = manifest.capabilities.as_ref().unwrap();
        assert!(capabilities.tools);
        assert!(!capabilities.approval);
        assert_eq!(capabilities.events, vec!["session.*", "tool.*"]);

        let contributions = manifest.ui_contributions();
        assert_eq!(contributions.len(), 2);

        let tab = &contributions[0];
        assert_eq!(tab.slot, "extension.tab");
        assert_eq!(tab.id, "board-tab");
        assert_eq!(tab.entry, "index.html");
        assert_eq!(tab.open_mode, OpenMode::Multi);
        assert_eq!(tab.sandbox, SandboxKind::Shadow);
        assert_eq!(tab.context, vec!["session", "workspace"]);

        // 未声明 open_mode/sandbox/context 时取缺省值
        let settings = &contributions[1];
        assert_eq!(settings.open_mode, OpenMode::Singleton);
        assert_eq!(settings.sandbox, SandboxKind::Shadow);
        assert!(settings.context.is_empty());
        assert_eq!(settings.title, "board-settings");
    }

    #[test]
    fn v2_非法_slot_被拒绝() {
        let json = v2_json().replace("\"extension.tab\"", "\"extension.unknown\"");
        let error = parse(&json).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("未知挂载点"));
        assert!(message.contains("extension.unknown"));
        assert!(message.contains("slot 无效"));
    }

    #[test]
    fn v2_open_mode_仅对_extension_tab_生效() {
        let json = v2_json().replace(
            "\"slot\": \"settings.plugin-page\",\n                        \"id\": \"board-settings\",\n                        \"entry\": \"settings.html\"",
            "\"slot\": \"settings.plugin-page\",\n                        \"id\": \"board-settings\",\n                        \"entry\": \"settings.html\",\n                        \"open_mode\": \"multi\"",
        );
        let error = parse(&json).unwrap_err();
        assert!(error.to_string().contains("open_mode"));
        assert!(error.to_string().contains("extension.tab"));
    }

    #[test]
    fn v2_非法_open_mode_取值被拒绝() {
        let json = v2_json().replace("\"open_mode\": \"multi\"", "\"open_mode\": \"always\"");
        let error = serde_json::from_str::<PluginManifest>(&json).unwrap_err();
        assert!(error.to_string().contains("always"));
    }

    #[test]
    fn v2_贡献级_sandbox_覆盖_默认级() {
        let json = v2_json().replace(
            "\"slot\": \"settings.plugin-page\"",
            "\"slot\": \"settings.plugin-page\",\n                        \"sandbox\": \"iframe\"",
        );
        let manifest = parse(&json).unwrap();
        let contributions = manifest.ui_contributions();
        assert_eq!(contributions[0].sandbox, SandboxKind::Shadow);
        assert_eq!(contributions[1].sandbox, SandboxKind::Iframe);
    }

    #[test]
    fn v2_ui_sandbox_默认级生效() {
        let json = v2_json().replace("\"sandbox\": \"shadow\"", "\"sandbox\": \"iframe\"");
        let manifest = parse(&json).unwrap();
        assert!(
            manifest
                .ui_contributions()
                .iter()
                .all(|item| item.sandbox == SandboxKind::Iframe)
        );
    }

    #[test]
    fn v2_native_沙箱需官方签名() {
        let manifest = parse(&v2_json()).unwrap();
        // 未签名：native 被拒
        let unsigned = v2_json().replace(
            "\"slot\": \"settings.plugin-page\"",
            "\"slot\": \"settings.plugin-page\",\n                        \"sandbox\": \"native\"",
        );
        let unsigned = parse(&unsigned).unwrap();
        let error = unsigned.validate_ui_native_sandbox(false).unwrap_err();
        assert!(error.to_string().contains("native"));
        assert!(error.to_string().contains("官方签名"));

        // 签名插件：native 放行
        assert!(manifest.validate_ui_native_sandbox(true).is_ok());

        // 非 native 沙箱不受签名影响
        assert!(unsigned.validate_ui_native_sandbox(true).is_ok());
    }

    #[test]
    fn v2_不支持的目标_slot_上下文被拒绝() {
        let json = v2_json().replace(
            "\"context\": [\"session\", \"workspace\"]",
            "\"context\": [\"session\", \"message\"]",
        );
        let error = parse(&json).unwrap_err();
        assert!(error.to_string().contains("message"));
        assert!(error.to_string().contains("不被"));
    }

    #[test]
    fn v2_未知字段被拒绝() {
        let json = v2_json().replace(
            "\"entry\": \"index.html\"",
            "\"entry\": \"index.html\", \"panel\": true",
        );
        let error = serde_json::from_str::<PluginManifest>(&json).unwrap_err();
        assert!(error.to_string().contains("panel"));
    }

    #[test]
    fn v2_贡献_id_重复被拒绝() {
        let json = v2_json().replace("\"id\": \"board-settings\"", "\"id\": \"board-tab\"");
        let error = parse(&json).unwrap_err();
        assert!(error.to_string().contains("重复"));
    }

    #[test]
    fn v2_entry_必须是安全相对路径() {
        for entry in ["../escape.html", "/abs.html"] {
            let json = v2_json().replace(
                "\"entry\": \"index.html\"",
                &format!("\"entry\": \"{entry}\""),
            );
            let error = parse(&json).unwrap_err();
            assert!(error.to_string().contains("安全的相对路径"));
        }
    }

    #[test]
    fn 不支持的_schema_版本被拒绝() {
        let json = v1_json().replace("\"schema_version\": 1", "\"schema_version\": 3");
        let error = parse(&json).unwrap_err();
        assert!(error.to_string().contains("清单版本不支持"));
    }

    #[test]
    fn v2_缺省_ui_等价无_ui_贡献() {
        let json = v1_json().replace("\"schema_version\": 1", "\"schema_version\": 2");
        let manifest = parse(&json).unwrap();
        assert!(manifest.ui_contributions().is_empty());
    }
}
