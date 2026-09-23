//! 六个桌面控制工具的请求与响应定义。
//!
//! 工具操作名统一带 `computer_use.` 前缀，避免多插件冲突。
//! 每个操作由零字段 marker struct 实现 `ComputerUseOperation`。

use serde::{Deserialize, Serialize};
use tiangong_types::InjectedAsset;

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

// ── desktop_screenshot（RFC 0017 图片注入的图源）────────────────

pub const DESKTOP_SCREENSHOT_OPERATION: &str = "computer_use.desktop_screenshot";

/// `desktop_screenshot` 工具请求。
///
/// 截取范围按优先级：显式 `region` > 应用窗口（app_name/pid/foreground_only
/// 任一提供即截取该应用最前窗口）> 主显示器全屏。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScreenshotRequest {
    /// 截取指定应用的窗口（包含匹配）；缺省截取整个主显示器。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 仅截取前台窗口。
    #[serde(default)]
    pub foreground_only: bool,
    /// 显式区域截图（屏幕逻辑坐标 points，主屏左上原点，与 desktop_snapshot
    /// 的 bounds 同系）；提供时忽略 app 定位。width/height 必须 > 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<crate::Bounds>,
    /// 产物长边最大像素（如 1568 为 Anthropic 推荐值）；超过时等比缩小
    /// 以节省视觉 token。缺省不缩放。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_dimension: Option<u32>,
    #[serde(flatten)]
    pub access: AccessContext,
}

/// `desktop_screenshot` 工具响应：图片落盘后的引用信息。
///
/// 工具结果文本只携带本结构（「知道」）；`injected_assets` 数组触发
/// core 的注入落地（「看见」，RFC 0017）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScreenshotResponse {
    /// 截图文件绝对路径（PNG）。
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// 截图目标应用名（全屏截图时为空或前台应用名）。
    #[serde(default)]
    pub app_name: String,
    pub size_bytes: u64,
    /// 注入声明：非空时 core 落成宿主注入的图片消息。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub injected_assets: Vec<InjectedAsset>,
}

pub struct Screenshot;
impl ComputerUseOperation for Screenshot {
    const NAME: &'static str = DESKTOP_SCREENSHOT_OPERATION;
    type Request = ScreenshotRequest;
    type Response = DesktopResult<ScreenshotResponse>;
}

// ── 生命周期 ──────────────────────────────────────────────────

// ── desktop_mouse（RFC 0018 §2.3：坐标级鼠标手势，CGEvent 合成）────
pub const DESKTOP_MOUSE_OPERATION: &str = "computer_use.desktop_mouse";
/// 鼠标手势类型。手势（而非裸事件）是暴露单元：down/up 永不单独出现，
/// drag 由实现侧生成插值轨迹保证时序，杜绝跨调用的按住状态泄漏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseGesture {
    /// 移动（hover）。
    Move,
    /// 左键单击。
    Click,
    /// 右键单击（唤出上下文菜单）。
    RightClick,
    /// 左键双击。
    DoubleClick,
    /// 按下后插值轨迹拖拽到终点再抬起。
    Drag,
    /// 滚轮（像素级）。
    Scroll,
}
/// `desktop_mouse` 工具请求。坐标为屏幕逻辑坐标（points，主屏左上原点，
/// 与 desktop_snapshot 的 bounds 同系）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseRequest {
    pub gesture: MouseGesture,
    /// 起点/目标点 X。
    pub x: f64,
    /// 起点/目标点 Y。
    pub y: f64,
    /// drag 终点 X（drag 必填）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_x: Option<f64>,
    /// drag 终点 Y（drag 必填）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_y: Option<f64>,
    /// 垂直滚动像素（正=向下；scroll 必填）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_y: Option<f64>,
    /// 水平滚动像素（正=向右；scroll 可选，默认 0）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_x: Option<f64>,
    #[serde(flatten)]
    pub access: AccessContext,
}
/// `desktop_mouse` 工具响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseResponse {
    pub performed: bool,
    pub summary: String,
}
pub struct Mouse;
impl ComputerUseOperation for Mouse {
    const NAME: &'static str = DESKTOP_MOUSE_OPERATION;
    type Request = MouseRequest;
    type Response = DesktopResult<MouseResponse>;
}
// ── desktop_keyboard（RFC 0018 §2.4：键盘合成输入，CGEvent）────────
pub const DESKTOP_KEYBOARD_OPERATION: &str = "computer_use.desktop_keyboard";
/// 键盘手势类型。`type` 走 Unicode 字符串分派（不经输入法、不依赖布局，
/// 中文与 ASCII 同路径）；`key`/`combo` 走虚拟键码（US ANSI 布局）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardActionKind {
    /// 输入一段文本（逐字符合成，目标为当前焦点控件）。
    #[default]
    Type,
    /// 按单个非修饰键（return/tab/esc/方向键/字母数字等）。
    Key,
    /// 组合键（修饰键 + 恰好一个非修饰键，如 cmd+c、cmd+shift+3）。
    Combo,
}
/// `desktop_keyboard` 工具请求。键盘事件投递给当前焦点应用：Agent 应先
/// 用 desktop_mouse click（或 desktop_action focus）建立输入焦点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyboardRequest {
    pub action: KeyboardActionKind,
    /// type 必填：要输入的文本（支持任意 Unicode）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// key 必填：键名（别名不敏感：enter/esc/backspace 等）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// combo 必填：键名数组，修饰键在前（如 ["cmd","c"]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
    #[serde(flatten)]
    pub access: AccessContext,
}
/// `desktop_keyboard` 工具响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardResponse {
    pub performed: bool,
    pub summary: String,
}
pub struct Keyboard;
impl ComputerUseOperation for Keyboard {
    const NAME: &'static str = DESKTOP_KEYBOARD_OPERATION;
    type Request = KeyboardRequest;
    type Response = DesktopResult<KeyboardResponse>;
}
// ── virtual_cursor（RFC 0018：Agent 可控的持久指针开关）──────────
pub const VIRTUAL_CURSOR_OPERATION: &str = "computer_use.virtual_cursor";
/// `virtual_cursor` 工具请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualCursorRequest {
    /// `true`：指针常驻显示并跟随 desktop_action 落点（出现在系统鼠标
    /// 当前位置）；`false`：淡出隐藏。
    pub enabled: bool,
}
/// `virtual_cursor` 工具响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualCursorResponse {
    /// 生效后的开关状态（回显）。
    pub enabled: bool,
}
pub struct VirtualCursor;
impl ComputerUseOperation for VirtualCursor {
    const NAME: &'static str = VIRTUAL_CURSOR_OPERATION;
    type Request = VirtualCursorRequest;
    type Response = DesktopResult<VirtualCursorResponse>;
}
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
