//! 六个桌面控制工具的请求与响应定义。
//!
//! 工具操作名统一带 `computer_use.` 前缀，避免多插件冲突。
//! 每个操作由零字段 marker struct 实现 `ComputerUseOperation`。

use serde::{Deserialize, Serialize};

use crate::{
    AccessibilityCapability, Ack, ActionKind, COMPUTER_USE_PROTOCOL_VERSION, ComputerUseOperation,
    ControlNode, DesktopError, DesktopResult, ElementRef, MatchMode, Platform, StableIdentifiers,
    WindowInfo,
};

pub const DESKTOP_STATUS_OPERATION: &str = "computer_use.desktop_status";
pub const DESKTOP_LIST_WINDOWS_OPERATION: &str = "computer_use.desktop_list_windows";
pub const DESKTOP_SNAPSHOT_OPERATION: &str = "computer_use.desktop_snapshot";
pub const DESKTOP_FIND_OPERATION: &str = "computer_use.desktop_find";
pub const DESKTOP_ACTION_OPERATION: &str = "computer_use.desktop_action";
pub const DESKTOP_WAIT_OPERATION: &str = "computer_use.desktop_wait";

/// 会话访问上下文：携带当前会话的信任模式，监督模式下动作需经用户批准。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccessContext {
    /// 是否完全信任模式（动作无需用户逐次批准）。
    #[serde(default)]
    pub full_trust: bool,
}

// ── desktop_status ────────────────────────────────────────────

/// `desktop_status` 工具请求：无参数，仅携带访问上下文。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesktopStatusRequest {
    #[serde(flatten)]
    pub access: AccessContext,
}

/// `desktop_status` 工具响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopStatusResponse {
    /// 当前编译目标平台。
    pub platform: Platform,
    /// 图形桌面会话状态。
    pub session: crate::DesktopSession,
    /// 无障碍能力是否可用及原因。
    pub accessibility: AccessibilityCapability,
    /// 受支持的动作类型集合。
    pub supported_actions: Vec<ActionKind>,
}

pub struct DesktopStatus;
impl ComputerUseOperation for DesktopStatus {
    const NAME: &'static str = DESKTOP_STATUS_OPERATION;
    type Request = DesktopStatusRequest;
    type Response = DesktopResult<DesktopStatusResponse>;
}

// ── desktop_list_windows ──────────────────────────────────────

/// `desktop_list_windows` 工具请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListWindowsRequest {
    /// 按应用名称筛选（包含匹配）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    /// 按进程编号筛选。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 仅返回前台窗口。
    #[serde(default)]
    pub foreground_only: bool,
    #[serde(flatten)]
    pub access: AccessContext,
}

/// `desktop_list_windows` 工具响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListWindowsResponse {
    pub windows: Vec<WindowInfo>,
}

pub struct ListWindows;
impl ComputerUseOperation for ListWindows {
    const NAME: &'static str = DESKTOP_LIST_WINDOWS_OPERATION;
    type Request = ListWindowsRequest;
    type Response = DesktopResult<ListWindowsResponse>;
}

// ── desktop_snapshot ──────────────────────────────────────────

/// 快照目标范围：必须提供应用或窗口之一。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotScope {
    /// 已知窗口引用（优先使用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<ElementRef>,
    /// 按应用名称/进程定位窗口（window 未提供时使用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// `desktop_snapshot` 工具请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotRequest {
    /// 目标范围，必须提供。
    pub scope: SnapshotScope,
    /// 最大遍历深度（默认按平台合理值，0 表示使用默认）。
    #[serde(default)]
    pub max_depth: u32,
    /// 最大节点数（0 表示使用默认）。
    #[serde(default)]
    pub max_nodes: u32,
    /// 是否包含不可见控件。
    #[serde(default)]
    pub include_invisible: bool,
    #[serde(flatten)]
    pub access: AccessContext,
}

/// `desktop_snapshot` 工具响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotResponse {
    /// 本次快照版本号，控件引用在其范围内有效。
    pub snapshot: u64,
    /// 控件节点表（含父子关系，平铺为列表，便于引用解析）。
    pub nodes: Vec<ControlNode>,
    /// 是否因深度/节点数/耗时限制而截断。
    pub truncated: bool,
    /// 单个控件读取失败时的告警（不影响整棵树）。
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub struct Snapshot;
impl ComputerUseOperation for Snapshot {
    const NAME: &'static str = DESKTOP_SNAPSHOT_OPERATION;
    type Request = SnapshotRequest;
    type Response = DesktopResult<SnapshotResponse>;
}

// ── desktop_find ──────────────────────────────────────────────

/// 查找条件：匹配优先级为“稳定标识 + 类型”，名称仅作补充。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FindConditions {
    /// 稳定标识（automation_id）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    /// 控件类型/角色。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// 名称或描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 当前值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// 是否可见。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// 是否可用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 是否拥有焦点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    /// 名称/值的匹配模式（不影响 automation_id/role 的精确匹配）。
    #[serde(default)]
    pub mode: MatchMode,
}

/// `desktop_find` 工具请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FindRequest {
    /// 查找范围：窗口引用或快照版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<ElementRef>,
    /// 若提供快照版本，则在该快照内查找，不重新读取界面。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<u64>,
    /// 查找条件。
    pub conditions: FindConditions,
    /// 最大返回候选数（0 表示使用默认）。
    #[serde(default)]
    pub max_candidates: u32,
    #[serde(flatten)]
    pub access: AccessContext,
}

/// `desktop_find` 工具响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FindResponse {
    /// 命中候选控件（含匹配摘要）。
    pub matches: Vec<ControlNode>,
    /// 快照版本。
    pub snapshot: u64,
    /// 是否存在多个候选（Agent 应明确指定后再操作）。
    pub ambiguous: bool,
}

pub struct Find;
impl ComputerUseOperation for Find {
    const NAME: &'static str = DESKTOP_FIND_OPERATION;
    type Request = FindRequest;
    type Response = DesktopResult<FindResponse>;
}

// ── desktop_action ────────────────────────────────────────────

/// `desktop_action` 工具请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionRequest {
    /// 目标控件引用。
    pub element: ElementRef,
    /// 要执行的动作。
    pub action: ActionRequestKind,
    /// set_value 动作的值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// select 动作的选项标识。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    #[serde(flatten)]
    pub access: AccessContext,
}

/// 动作请求（与 ActionKind 对齐，但单独定义以便序列化携带额外字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActionRequestKind {
    #[default]
    Focus,
    Press,
    SetValue,
    Toggle,
    Select,
    Expand,
    Collapse,
    ScrollIntoView,
}

impl From<ActionRequestKind> for ActionKind {
    fn from(value: ActionRequestKind) -> Self {
        match value {
            ActionRequestKind::Focus => Self::Focus,
            ActionRequestKind::Press => Self::Press,
            ActionRequestKind::SetValue => Self::SetValue,
            ActionRequestKind::Toggle => Self::Toggle,
            ActionRequestKind::Select => Self::Select,
            ActionRequestKind::Expand => Self::Expand,
            ActionRequestKind::Collapse => Self::Collapse,
            ActionRequestKind::ScrollIntoView => Self::ScrollIntoView,
        }
    }
}

/// `desktop_action` 工具响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionResponse {
    /// 动作是否已执行（不代表界面已变化，由后续 wait/snapshot 确认）。
    pub performed: bool,
    /// 执行后控件状态摘要。
    pub summary: String,
    /// 动作打开新顶层窗口或新进程时返回新窗口引用，需重新从桌面根节点发现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_window: Option<ElementRef>,
}

pub struct Action;
impl ComputerUseOperation for Action {
    const NAME: &'static str = DESKTOP_ACTION_OPERATION;
    type Request = ActionRequest;
    type Response = DesktopResult<ActionResponse>;
}

// ── desktop_wait ──────────────────────────────────────────────

/// 等待条件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitCondition {
    /// 窗口或控件出现。
    Appear { target: WaitTarget },
    /// 窗口或控件消失。
    Disappear { target: WaitTarget },
    /// 控件获得焦点。
    Focus { element: ElementRef },
    /// 控件可用状态变化（变为 enabled）。
    Available { element: ElementRef },
    /// 控件值变化（可选期望值）。
    Value {
        element: ElementRef,
        expected: Option<String>,
    },
}

/// 等待目标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// `desktop_wait` 工具请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitRequest {
    pub condition: WaitCondition,
    /// 超时毫秒数，必须 > 0。
    pub timeout_ms: u64,
    #[serde(flatten)]
    pub access: AccessContext,
}

/// `desktop_wait` 工具响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WaitResponse {
    pub satisfied: bool,
    /// 实际等待毫秒数。
    pub waited_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_element: Option<ElementRef>,
}

pub struct Wait;
impl ComputerUseOperation for Wait {
    const NAME: &'static str = DESKTOP_WAIT_OPERATION;
    type Request = WaitRequest;
    type Response = DesktopResult<WaitResponse>;
}

// ── 生命周期 ──────────────────────────────────────────────────

pub const SET_ACCESS_OPERATION: &str = "computer_use.set_access";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetAccessRequest {
    #[serde(default)]
    pub full_trust: bool,
}

pub struct SetAccess;
impl ComputerUseOperation for SetAccess {
    const NAME: &'static str = SET_ACCESS_OPERATION;
    type Request = SetAccessRequest;
    type Response = Ack;
}

/// 握手响应中声明的能力标识。
pub const CAPABILITY: &str = "computer-use";
/// 握手响应返回的业务协议版本（sidecar service 直接引用，避免与常量漂移）。
pub fn handshake_business_protocol() -> u32 {
    COMPUTER_USE_PROTOCOL_VERSION
}
/// 握手响应中声明的稳定标识占位（sidecar 按平台填充实际值）。
pub fn stable_identifiers_placeholder() -> StableIdentifiers {
    StableIdentifiers::default()
}

/// 用于错误响应构造的便捷类型（sidecar service 内部使用）。
pub type OperationError = DesktopError;
