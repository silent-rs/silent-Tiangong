//! 平台无障碍后端抽象与各平台实现。
//!
//! trait [`Backend`] 定义统一的桌面访问能力，各平台按条件编译提供实现：
//! - macOS：通过 objc2 探测图形会话与辅助功能授权，列举运行中的应用窗口。
//! - Windows / Linux：当前制品返回明确的能力不足结果，运行时不影响宿主启动。

use async_trait::async_trait;

use tiangong_plugin_computer_use_protocol::ops::{
    ActionRequest, FindRequest, ListWindowsRequest, SnapshotRequest, WaitRequest,
};
use tiangong_plugin_computer_use_protocol::{
    AccessibilityCapability, ActionKind, DesktopResult, DesktopSession, ListWindowsResponse,
    Platform,
};

/// 平台无障碍后端能力。
///
/// 所有方法返回 `DesktopResult<T>`，业务错误统一由 [`DesktopError`] 表达，
/// 便于上层序列化回 WASM 侧。
#[async_trait]
pub trait Backend: Send + Sync {
    /// 当前编译目标平台。
    fn platform(&self) -> Platform;

    /// 探测图形会话与无障碍能力，返回受支持的动作集合。
    async fn status(&self) -> DesktopResult<StatusInfo>;

    /// 列出当前可访问的应用和顶层窗口。
    async fn list_windows(&self, req: &ListWindowsRequest) -> DesktopResult<ListWindowsResponse>;

    /// 读取控件树快照。
    async fn snapshot(&self, req: &SnapshotRequest) -> DesktopResult<SnapshotInfo>;

    /// 在窗口或快照内查找控件。
    async fn find(&self, req: &FindRequest) -> DesktopResult<FindInfo>;

    /// 对控件执行动作。
    async fn action(&self, req: &ActionRequest) -> DesktopResult<ActionResult>;

    /// 等待条件满足。
    async fn wait(&self, req: &WaitRequest) -> DesktopResult<WaitResult>;
}

/// `desktop_status` 返回信息。
#[derive(Debug, Clone)]
pub struct StatusInfo {
    pub session: DesktopSession,
    pub accessibility: AccessibilityCapability,
    pub supported_actions: Vec<ActionKind>,
}

/// `desktop_snapshot` 返回信息（节点平铺）。
#[derive(Debug, Clone, Default)]
pub struct SnapshotInfo {
    pub snapshot: u64,
    pub nodes: Vec<tiangong_plugin_computer_use_protocol::ControlNode>,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

/// `desktop_find` 返回信息。
#[derive(Debug, Clone, Default)]
pub struct FindInfo {
    pub matches: Vec<tiangong_plugin_computer_use_protocol::ControlNode>,
    pub snapshot: u64,
    pub ambiguous: bool,
}

/// `desktop_action` 返回信息。
#[derive(Debug, Clone, Default)]
pub struct ActionResult {
    pub performed: bool,
    pub summary: String,
    pub new_window: Option<tiangong_plugin_computer_use_protocol::ElementRef>,
}

/// `desktop_wait` 返回信息。
#[derive(Debug, Clone, Default)]
pub struct WaitResult {
    pub satisfied: bool,
    pub waited_ms: u64,
    pub matched_element: Option<tiangong_plugin_computer_use_protocol::ElementRef>,
}

/// 三平台均受支持的动作集合（统一暴露给 Agent）。
pub fn all_supported_actions() -> Vec<ActionKind> {
    use ActionKind::*;
    vec![
        Focus,
        Press,
        SetValue,
        Toggle,
        Select,
        Expand,
        Collapse,
        ScrollIntoView,
    ]
}

/// 在运行时构造当前平台的后端实例。
pub fn current_backend() -> Box<dyn Backend> {
    cfg_if_current_backend()
}

// ── 平台分发 ───────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn cfg_if_current_backend() -> Box<dyn Backend> {
    Box::new(crate::backend::macos::MacosBackend::new())
}

#[cfg(target_os = "windows")]
fn cfg_if_current_backend() -> Box<dyn Backend> {
    Box::new(crate::backend::stub::StubBackend::unsupported(
        Platform::Windows,
    ))
}

#[cfg(target_os = "linux")]
fn cfg_if_current_backend() -> Box<dyn Backend> {
    Box::new(crate::backend::linux::LinuxBackend::new())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn cfg_if_current_backend() -> Box<dyn Backend> {
    Box::new(crate::backend::stub::StubBackend::unsupported(
        Platform::Windows,
    ))
}

// ── 各平台后端 ─────────────────────────────────────────────────

#[cfg(target_os = "macos")]
pub mod ax;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

/// 通用存根后端：用于尚未实现原生能力的平台，返回明确的能力不足结果。
#[cfg(any(
    target_os = "windows",
    not(any(target_os = "macos", target_os = "windows", target_os = "linux"))
))]
pub mod stub;
