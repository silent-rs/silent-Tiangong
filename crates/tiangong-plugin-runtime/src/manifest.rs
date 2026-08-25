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
    /// 逻辑层 WASM 制品。schema v2 可省略——纯 UI 插件（无工具/生命周期等
    /// 逻辑能力）经宿主桥接（storage.* 等）即可工作，见设计文档 9.1。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm: Option<WasmManifest>,
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
    /// Desktop 纯 TypeScript 插件声明的工具规格。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<TsToolDecl>>,
    /// 系统提示注入段落。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Vec<String>>,
    /// 随插件分发的静态资源目录（相对插件目录，导入时递归复制进安装目录）。
    ///
    /// 供插件携带模板、示例等只读资产；不得声明宿管保留目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Vec<String>>,
    /// @提及声明：声明后插件出现在输入框 @ 候选中，用户可点名调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mention: Option<MentionManifest>,
}

/// @提及声明（`mention` 字段）。
///
/// 候选 value/label 由宿主从插件 id 与 UI 贡献标题推导，插件只声明展示
/// 副标题（hint）——@skill / @mcp 同款交互，用户点名后 Agent 按插件
/// 说明（prompt）使用能力。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MentionManifest {
    /// 候选副标题（一句话能力描述）。
    pub hint: String,
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
    /// 处理交互接缝（表单/选择/填写）。
    #[serde(default)]
    pub interaction: bool,
    /// 订阅的事件命名空间（如 `session.*`、`tool.*`）。
    #[serde(default)]
    pub events: Vec<String>,
}

fn default_ts_tool_timeout_ms() -> u64 {
    20_000
}

/// Desktop 纯 TypeScript 插件工具声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TsToolDecl {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// 前端插件崩溃或未响应时的宿主兜底上限。
    #[serde(default = "default_ts_tool_timeout_ms")]
    pub timeout_ms: u64,
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

/// sidecar 的运行时形态：原生二进制或解释器（宿主白名单分派）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SidecarRuntime {
    /// 插件目录内的原生可执行文件（存量默认，要求官方签名）。
    #[default]
    Native,
    /// 系统解释器运行 `entry` 脚本（本地信任放行）。
    Node,
    Python,
}

/// sidecar 进程生命周期：按需调用即起即清，常驻跨调用复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SidecarLifecycle {
    /// 每次调用独立进程（spawn → 握手 → 请求 → 清理）；无进程内状态，
    /// 存活窗口最小。工具型调用的安全默认；推送/长连接不可用。
    #[default]
    OnDemand,
    /// 常驻复用（懒启动、崩溃换代重启、通知推送可用）。
    Resident,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarManifest {
    /// 相对插件目录的可执行文件名，不包含平台可执行后缀（native 形态必填）。
    #[serde(default)]
    pub binary: Option<PathBuf>,
    /// 运行时形态；非 native 时由宿主以解释器启动 entry，不接受任意命令。
    #[serde(default)]
    pub runtime: SidecarRuntime,
    /// 解释器入口脚本（相对插件目录的安全相对路径；非 native 形态必填）。
    #[serde(default)]
    pub entry: Option<PathBuf>,
    /// 传给入口的固定参数（受数量与长度约束）。
    #[serde(default)]
    pub args: Vec<String>,
    /// 进程生命周期（按需默认；常驻需显式声明）。
    #[serde(default)]
    pub lifecycle: SidecarLifecycle,
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
        let manifest: Self = serde_json::from_str(&content).map_err(|error| {
            // 未知字段/类型不匹配等 serde 细节是排障关键，完整保留进错误链
            anyhow::anyhow!("清单字段不符合 schema（路径 {}）：{error}", path.display())
        })?;
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
        match self.wasm_binary() {
            Some(binary) => validate_relative_path(binary, "wasm.binary")?,
            None => {
                // 纯 UI 插件：无逻辑层时必须有界面贡献，且仅 v2 支持
                if self.schema_version != MANIFEST_SCHEMA_VERSION_V2 {
                    bail!(
                        "插件 {} 未声明 wasm（仅 schema_version 2 支持纯 UI 插件）",
                        self.id
                    );
                }
                if self.ui_contributions().is_empty() {
                    bail!(
                        "插件 {} 未声明 wasm 时必须至少声明一条 ui.contributions",
                        self.id
                    );
                }
            }
        }
        if let Some(sidecar) = &self.sidecar {
            if sidecar.transport_protocol.trim().is_empty() {
                bail!("插件 {} sidecar transport 版本为空", self.id);
            }
            if sidecar.startup_timeout_ms == 0 || sidecar.request_timeout_ms == 0 {
                bail!("插件 {} sidecar 超时时间必须大于 0", self.id);
            }
            match sidecar.runtime {
                SidecarRuntime::Native => {
                    let binary = sidecar.binary.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("插件 {} native sidecar 必须声明 binary", self.id)
                    })?;
                    validate_relative_path(binary, "sidecar.binary")?;
                    if sidecar.entry.is_some() {
                        bail!("插件 {} native sidecar 不应声明 entry", self.id);
                    }
                }
                SidecarRuntime::Node | SidecarRuntime::Python => {
                    let entry = sidecar.entry.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "插件 {} 解释器 sidecar（{:?}）必须声明 entry",
                            self.id,
                            sidecar.runtime
                        )
                    })?;
                    validate_relative_path(entry, "sidecar.entry")?;
                    // 入口所在目录整树进入安装目录（协议库等随行文件），入口必须位于子目录。
                    if entry
                        .parent()
                        .is_none_or(|parent| parent.as_os_str().is_empty())
                    {
                        bail!("插件 {} 解释器 sidecar entry 必须位于子目录内", self.id);
                    }
                    if sidecar.binary.is_some() {
                        bail!(
                            "插件 {} 解释器 sidecar 不允许声明 binary（解释器由宿主选择）",
                            self.id
                        );
                    }
                }
            }
            if sidecar.args.len() > 16 {
                bail!("插件 {} sidecar.args 数量超过上限 16", self.id);
            }
            if sidecar
                .args
                .iter()
                .any(|arg| arg.is_empty() || arg.len() > 512)
            {
                bail!("插件 {} sidecar.args 包含空值或超过 512 字符的项", self.id);
            }
        }
        if self.permissions.iter().any(|item| item.trim().is_empty()) {
            bail!("插件 {} permissions 不能包含空值", self.id);
        }
        if let Some(mention) = &self.mention
            && mention.hint.trim().is_empty()
        {
            bail!("插件 {} mention.hint 不能为空", self.id);
        }
        if let Some(resources) = &self.resources {
            for directory in resources {
                validate_relative_path(Path::new(directory), "resources")?;
                if Path::new(directory)
                    .components()
                    .next()
                    .is_some_and(|component| {
                        matches!(
                            component.as_os_str().to_str(),
                            Some("runtime" | "logs" | "data")
                        )
                    })
                {
                    bail!(
                        "插件 {} resources 不能声明宿管保留目录 {directory}",
                        self.id
                    );
                }
            }
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
            if self.tools.is_some() || self.prompt.is_some() {
                bail!(
                    "插件 {} 使用 schema_version 1 但声明了 tools/prompt 字段，请升级 schema_version 为 {MANIFEST_SCHEMA_VERSION_V2}",
                    self.id
                );
            }
            if self.resources.is_some() {
                bail!(
                    "插件 {} 使用 schema_version 1 但声明了 resources 字段，请升级 schema_version 为 {MANIFEST_SCHEMA_VERSION_V2}",
                    self.id
                );
            }
            if self.mention.is_some() {
                bail!(
                    "插件 {} 使用 schema_version 1 但声明了 mention 字段，请升级 schema_version 为 {MANIFEST_SCHEMA_VERSION_V2}",
                    self.id
                );
            }
        } else {
            self.validate_v2()?;
            self.validate_ts_tools()?;
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

    fn validate_ts_tools(&self) -> Result<()> {
        let tools = self.tools.as_deref().unwrap_or_default();
        let prompts = self.prompt.as_deref().unwrap_or_default();
        if !tools.is_empty() || !prompts.is_empty() {
            if self.wasm_binary().is_some() {
                bail!(
                    "插件 {} 不能同时声明 WASM 与纯 TypeScript tools/prompt",
                    self.id
                );
            }
            if !self
                .entrypoints
                .as_ref()
                .is_some_and(|items| items.len() == 1 && items[0] == "desktop")
            {
                bail!("纯 TypeScript 工具插件 {} 只能声明 desktop 入口", self.id);
            }
        }
        if !tools.is_empty() {
            if !self.has_permission("tool.provide") {
                bail!("插件 {} 声明 tools 时必须声明 tool.provide 权限", self.id);
            }
            if !self
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.tools)
            {
                bail!("插件 {} 声明 tools 时必须启用 capabilities.tools", self.id);
            }
        }

        let mut names = BTreeSet::new();
        for tool in tools {
            if tool.name.trim().is_empty()
                || !tool
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                bail!(
                    "插件 {} 工具名 {} 无效（字母数字与 - _）",
                    self.id,
                    tool.name
                );
            }
            if !names.insert(tool.name.as_str()) {
                bail!("插件 {} 包含重复工具名 {}", self.id, tool.name);
            }
            if tool
                .input_schema
                .get("type")
                .and_then(|value| value.as_str())
                != Some("object")
            {
                bail!(
                    "插件 {} 工具 {} 的 input_schema 必须为 object",
                    self.id,
                    tool.name
                );
            }
            if !(1_000..=300_000).contains(&tool.timeout_ms) {
                bail!(
                    "插件 {} 工具 {} 的 timeout_ms 必须在 1000 到 300000 之间",
                    self.id,
                    tool.name
                );
            }
        }

        if !prompts.is_empty() {
            if !self
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.prompt)
            {
                bail!(
                    "插件 {} 声明 prompt 时必须启用 capabilities.prompt",
                    self.id
                );
            }
            if prompts.iter().any(|section| section.trim().is_empty()) {
                bail!("插件 {} prompt 段落不能为空", self.id);
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

    pub fn wasm_binary(&self) -> Option<&Path> {
        match &self.wasm {
            Some(WasmManifest::Detailed { binary }) | Some(WasmManifest::Legacy(binary)) => {
                Some(binary)
            }
            None => None,
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
    fn mention声明_校验() {
        let json = r#"{"schema_version":2,"id":"m-demo","version":"0.1.0","permissions":[],"mention":{"hint":"问候能力"},"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"app/index.html"}]}}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        manifest.validate().unwrap();
        // 空 hint 拒绝
        let bad = json.replace("问候能力", "  ");
        let manifest: PluginManifest = serde_json::from_str(&bad).unwrap();
        assert!(manifest.validate().is_err());
        // v1 清单拒绝
        let legacy = json.replace("\"schema_version\":2", "\"schema_version\":1");
        let manifest: PluginManifest = serde_json::from_str(&legacy).unwrap();
        assert!(manifest.validate().is_err());
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
    fn 纯_ui_插件_wasm_可省略但须有贡献() {
        // v2 + ui 贡献：wasm 可省略
        let manifest: PluginManifest = serde_json::from_str(
            r#"{"schema_version":2,"id":"com.example.board","version":"1.0.0","permissions":["bridge.call"],"ui":{"contributions":[{"slot":"extension.tab","id":"board","entry":"index.html"}]}}"#,
        )
        .unwrap();
        manifest.validate().expect("纯 UI 插件应通过校验");
        assert!(manifest.wasm_binary().is_none());
        assert_eq!(manifest.ui_contributions().len(), 1);

        // v2 + 无 wasm + 无 ui 贡献：拒绝
        let bare: PluginManifest = serde_json::from_str(
            r#"{"schema_version":2,"id":"com.example.bare","version":"1.0.0"}"#,
        )
        .unwrap();
        let error = bare.validate().unwrap_err();
        assert!(format!("{error:#}").contains("ui.contributions"));

        // v1 + 无 wasm：拒绝（纯 UI 仅 v2 支持）
        let v1_bare: PluginManifest = serde_json::from_str(
            r#"{"schema_version":1,"id":"a","version":"1.0.0","ui":{"contributions":[{"slot":"settings.plugin-page","id":"s","entry":"i.html"}]}}"#,
        )
        .unwrap();
        let error = v1_bare.validate().unwrap_err();
        // v1 携带 ui 字段先被 v2 字段校验拒绝（提示升级 schema_version）
        assert!(format!("{error:#}").contains("schema_version"));
    }

    #[test]
    fn v2_缺省_ui_等价无_ui_贡献() {
        let json = v1_json().replace("\"schema_version\": 1", "\"schema_version\": 2");
        let manifest = parse(&json).unwrap();
        assert!(manifest.ui_contributions().is_empty());
    }

    fn sidecar_json(sidecar: &str) -> String {
        format!(
            r#"{{"schema_version":2,"id":"com.example.sc","version":"1.0.0","permissions":["sidecar.invoke"],"ui":{{"contributions":[{{"slot":"extension.tab","id":"app","entry":"app/index.html"}}]}},"sidecar":{sidecar}}}"#
        )
    }

    #[test]
    fn 解释器_sidecar_声明解析与校验() {
        let manifest: PluginManifest = serde_json::from_str(&sidecar_json(
            r#"{"runtime":"node","entry":"sidecar/main.mjs"}"#,
        ))
        .unwrap();
        manifest.validate().expect("解释器 sidecar 声明应通过校验");
        let sidecar = manifest.sidecar.as_ref().unwrap();
        assert_eq!(sidecar.runtime, SidecarRuntime::Node);
        assert_eq!(
            sidecar.entry.as_deref(),
            Some(std::path::Path::new("sidecar/main.mjs"))
        );
        assert!(sidecar.binary.is_none());
    }

    #[test]
    fn 解释器_sidecar_缺少_entry_被拒绝() {
        let manifest: PluginManifest =
            serde_json::from_str(&sidecar_json(r#"{"runtime":"python"}"#)).unwrap();
        let error = manifest.validate().unwrap_err();
        assert!(format!("{error:#}").contains("必须声明 entry"));
    }

    #[test]
    fn 解释器_sidecar_不允许声明_binary() {
        let manifest: PluginManifest = serde_json::from_str(&sidecar_json(
            r#"{"runtime":"node","entry":"sidecar/main.mjs","binary":"x"}"#,
        ))
        .unwrap();
        let error = manifest.validate().unwrap_err();
        assert!(format!("{error:#}").contains("不允许声明 binary"));
    }

    #[test]
    fn 解释器_sidecar_entry_必须在子目录() {
        let manifest: PluginManifest =
            serde_json::from_str(&sidecar_json(r#"{"runtime":"node","entry":"main.mjs"}"#))
                .unwrap();
        let error = manifest.validate().unwrap_err();
        assert!(format!("{error:#}").contains("必须位于子目录"));
    }

    #[test]
    fn 解释器_sidecar_entry_路径逃逸被拒绝() {
        let manifest: PluginManifest =
            serde_json::from_str(&sidecar_json(r#"{"runtime":"node","entry":"../main.mjs"}"#))
                .unwrap();
        let error = manifest.validate().unwrap_err();
        assert!(format!("{error:#}").contains("安全的相对路径"));
    }

    #[test]
    fn native_sidecar_缺_binary_被拒绝() {
        let manifest: PluginManifest = serde_json::from_str(&sidecar_json(r#"{}"#)).unwrap();
        let error = manifest.validate().unwrap_err();
        assert!(format!("{error:#}").contains("必须声明 binary"));

        // 缺省 runtime 等价 native（存量清单行为不变）
        let manifest: PluginManifest =
            serde_json::from_str(&sidecar_json(r#"{"binary":"sidecar-bin"}"#)).unwrap();
        manifest.validate().expect("缺省 runtime 解析为 native");
        assert_eq!(
            manifest.sidecar.as_ref().unwrap().runtime,
            SidecarRuntime::Native
        );
    }

    #[test]
    fn 未知_runtime_值解析失败() {
        let result: Result<PluginManifest, _> = serde_json::from_str(&sidecar_json(
            r#"{"runtime":"npx","entry":"tools/main.ts"}"#,
        ));
        assert!(result.is_err());
    }
}
