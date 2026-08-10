//! Windows UI Automation 无障碍后端。
//!
//! 通过 `uiautomation` crate（基于 windows-rs 的 UIA COM 封装）访问桌面控件树，
//! 列举应用窗口、读取控件树、查找控件、执行动作并等待状态变化。
//!
//! 从桌面根节点按 ControlType 与进程缩小范围，优先使用 Control View，
//! 避免无边界遍历整个桌面。目标窗口属于更高权限进程或安全桌面时返回权限受限，
//! 不提升权限绕过。动作按控件真实支持的 pattern 调用，动作前重新确认控件身份。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use uiautomation::controls::ControlType;
use uiautomation::core::{UIAutomation, UIElement};
use uiautomation::patterns::{
    UIExpandCollapsePattern, UIInvokePattern, UIScrollItemPattern, UISelectionItemPattern,
    UITogglePattern, UIValuePattern,
};
use uiautomation::types::TreeScope;

use super::{ActionResult, Backend, FindInfo, SnapshotInfo, StatusInfo, WaitResult};
use tiangong_plugin_computer_use_protocol::ops::{
    ActionRequest, FindConditions, FindRequest, ListWindowsRequest, SnapshotRequest, WaitCondition,
    WaitRequest,
};
use tiangong_plugin_computer_use_protocol::{
    AccessibilityCapability, ActionKind, Bounds, ControlNode, DesktopError, DesktopResult,
    DesktopSession, ElementRef, MatchMode, Platform, StableIdentifiers, WindowInfo,
};

const DEFAULT_MAX_DEPTH: u32 = 8;
const DEFAULT_MAX_NODES: usize = 400;

pub struct WindowsBackend {
    snapshot_seq: AtomicU64,
    /// 快照内控件引用缓存：((snapshot, id) -> UIElement)。
    /// UIElement 内部持有 COM 对象引用，Clone 增引用计数。
    elements: RwLock<HashMap<(u64, String), UIElement>>,
    /// `list_windows` 返回的真实窗口引用，避免后续按易变的枚举序号重新猜测目标。
    window_elements: RwLock<HashMap<(u64, String), UIElement>>,
    /// 快照节点表缓存：(snapshot -> Vec<ControlNode>)，供 find 筛选。
    snapshot_nodes: RwLock<HashMap<u64, Vec<ControlNode>>>,
    /// 已使用的快照版本，用于淘汰旧缓存。
    recent_snapshots: Mutex<Vec<u64>>,
    /// 最近的窗口列表版本；窗口引用与控件快照分别管理。
    recent_window_snapshots: Mutex<Vec<u64>>,
}

impl Default for WindowsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsBackend {
    pub fn new() -> Self {
        Self {
            snapshot_seq: AtomicU64::new(1),
            elements: RwLock::new(HashMap::new()),
            window_elements: RwLock::new(HashMap::new()),
            snapshot_nodes: RwLock::new(HashMap::new()),
            recent_snapshots: Mutex::new(Vec::new()),
            recent_window_snapshots: Mutex::new(Vec::new()),
        }
    }

    fn next_snapshot(&self) -> u64 {
        self.snapshot_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// 记录快照版本，保留最近 4 个，清理更早的缓存。
    fn remember_snapshot(&self, snapshot: u64) {
        let mut recent = self.recent_snapshots.lock().unwrap();
        recent.retain(|&s| s != snapshot);
        recent.push(snapshot);
        while recent.len() > 4 {
            let old = recent.remove(0);
            self.elements.write().unwrap().retain(|(s, _), _| *s != old);
            self.snapshot_nodes.write().unwrap().remove(&old);
        }
    }

    /// 记录窗口列表版本，保留最近 4 次枚举返回的真实窗口对象。
    fn remember_window_snapshot(&self, snapshot: u64) {
        let mut recent = self.recent_window_snapshots.lock().unwrap();
        recent.retain(|&s| s != snapshot);
        recent.push(snapshot);
        while recent.len() > 4 {
            let old = recent.remove(0);
            self.window_elements
                .write()
                .unwrap()
                .retain(|(s, _), _| *s != old);
        }
    }

    /// 创建 UIA 实例。失败说明无图形会话或 UIA 服务不可用。
    fn automation() -> Result<UIAutomation, DesktopError> {
        UIAutomation::new().map_err(|e| DesktopError::DesktopSessionUnavailable {
            reason: format!("无法初始化 UI Automation: {e}"),
        })
    }

    /// 读取 UIElement 边界并转为 Bounds（读取失败返回默认）。
    fn bounds_of(element: &UIElement) -> Bounds {
        element
            .get_bounding_rectangle()
            .map_or(Bounds::default(), |rect| Bounds {
                x: rect.get_left() as f64,
                y: rect.get_top() as f64,
                width: rect.get_width() as f64,
                height: rect.get_height() as f64,
            })
    }

    /// 列举顶层窗口元素。
    fn top_level_windows(automation: &UIAutomation) -> Result<Vec<UIElement>, DesktopError> {
        let root = automation
            .get_root_element()
            .map_err(|e| DesktopError::BackendUnavailable {
                reason: format!("获取桌面根元素失败: {e}"),
            })?;
        let condition = automation
            .create_property_condition(
                uiautomation::types::UIProperty::ControlType,
                uiautomation::variants::Variant::from(ControlType::Window as i32),
                None,
            )
            .map_err(|e| DesktopError::BackendUnavailable {
                reason: format!("创建窗口筛选条件失败: {e}"),
            })?;
        root.find_all(TreeScope::Children, &condition).map_err(|e| {
            DesktopError::BackendUnavailable {
                reason: format!("查找顶层窗口失败: {e}"),
            }
        })
    }

    /// 探测控件真实支持的 pattern，返回对应的统一动作集合。
    fn detect_actions(element: &UIElement) -> Vec<ActionKind> {
        use ActionKind::*;
        let mut actions = Vec::new();
        // 控件总是可以尝试获得焦点。
        actions.push(Focus);
        if element.get_pattern::<UIInvokePattern>().is_ok() {
            actions.push(Press);
        }
        if element.get_pattern::<UIValuePattern>().is_ok() {
            actions.push(SetValue);
        }
        if element.get_pattern::<UITogglePattern>().is_ok() {
            actions.push(Toggle);
        }
        if element.get_pattern::<UISelectionItemPattern>().is_ok() {
            actions.push(Select);
        }
        if element.get_pattern::<UIExpandCollapsePattern>().is_ok() {
            actions.push(Expand);
            actions.push(Collapse);
        }
        if element.get_pattern::<UIScrollItemPattern>().is_ok() {
            actions.push(ScrollIntoView);
        }
        actions
    }

    /// 校验控件身份：automation_id、control_type、process_id 是否与快照时一致。
    /// 任一不一致返回 false（控件已变化，不应继续操作）。
    fn verify_identity(element: &UIElement, node: &ControlNode) -> bool {
        // automation_id 一致性（控件稳定标识）。
        if let Some(expected_id) = &node.identifiers.automation_id {
            let actual = element.get_automation_id().unwrap_or_default();
            if actual.is_empty() || &actual != expected_id {
                return false;
            }
        }
        // control_type 一致性。
        let expected_type = node.identifiers.role.as_deref().unwrap_or("");
        let actual_type = format!(
            "{:?}",
            element.get_control_type().unwrap_or(ControlType::Custom)
        );
        if !expected_type.is_empty() && actual_type != expected_type {
            return false;
        }
        true
    }
}

#[async_trait]
impl Backend for WindowsBackend {
    fn platform(&self) -> Platform {
        Platform::Windows
    }

    async fn status(&self) -> DesktopResult<StatusInfo> {
        match Self::automation() {
            Ok(_uia) => DesktopResult::Ok(StatusInfo {
                session: DesktopSession::Available,
                accessibility: AccessibilityCapability {
                    available: true,
                    reason: None,
                },
                supported_actions: {
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
                },
            }),
            Err(e) => DesktopResult::Ok(StatusInfo {
                session: DesktopSession::Unavailable,
                accessibility: AccessibilityCapability {
                    available: false,
                    reason: Some(e.agent_message().to_string()),
                },
                supported_actions: Vec::new(),
            }),
        }
    }

    async fn list_windows(
        &self,
        req: &ListWindowsRequest,
    ) -> DesktopResult<tiangong_plugin_computer_use_protocol::ListWindowsResponse> {
        let automation = match Self::automation() {
            Ok(a) => a,
            Err(e) => return DesktopResult::Err(e),
        };
        let elements = match Self::top_level_windows(&automation) {
            Ok(ws) => ws,
            Err(e) => return DesktopResult::Err(e),
        };
        let snapshot = self.next_snapshot();
        let mut windows = Vec::new();
        for (idx, element) in elements.into_iter().enumerate() {
            let name = element.get_name().unwrap_or_default();
            let pid = element.get_process_id().unwrap_or(0);
            let app_name = if name.is_empty() {
                format!("进程 {pid}")
            } else {
                name.clone()
            };
            let id = format!("uia-win-{pid}-{idx}");
            self.window_elements
                .write()
                .unwrap()
                .insert((snapshot, id.clone()), element.clone());
            windows.push(WindowInfo {
                app_name: app_name.clone(),
                pid: pid as u32,
                element: ElementRef { id, snapshot },
                title: name,
                is_foreground: false,
                bounds: Self::bounds_of(&element),
                visible: true,
                enabled: true,
                identifiers: StableIdentifiers {
                    automation_id: element.get_automation_id().ok(),
                    role: Some("Window".to_string()),
                },
            });
        }
        self.remember_window_snapshot(snapshot);
        if let Some(app_name) = req.app_name.as_deref() {
            let needle = app_name.to_lowercase();
            windows.retain(|w| w.app_name.to_lowercase().contains(&needle));
        }
        if let Some(pid) = req.pid {
            windows.retain(|w| w.pid == pid);
        }
        if req.foreground_only {
            return DesktopResult::Err(DesktopError::BackendUnavailable {
                reason: "Windows 后端暂不支持 foreground_only 筛选".to_string(),
            });
        }
        DesktopResult::Ok(tiangong_plugin_computer_use_protocol::ListWindowsResponse { windows })
    }

    async fn snapshot(&self, req: &SnapshotRequest) -> DesktopResult<SnapshotInfo> {
        let automation = match Self::automation() {
            Ok(a) => a,
            Err(e) => return DesktopResult::Err(e),
        };
        let windows = match Self::top_level_windows(&automation) {
            Ok(ws) => ws,
            Err(e) => return DesktopResult::Err(e),
        };
        // 定位根元素：窗口引用必须从 list_windows 缓存还原真实 UIElement，不能依赖
        // 两次枚举之间可能变化的窗口顺序。其次才允许按 pid 或 app_name 重新发现。
        let root = if let Some(window_ref) = &req.scope.window {
            let cached = self
                .window_elements
                .read()
                .unwrap()
                .get(&(window_ref.snapshot, window_ref.id.clone()))
                .cloned();
            match cached {
                Some(window) if window.get_process_id().is_ok() => window,
                _ => {
                    return DesktopResult::Err(DesktopError::StaleElement {
                        snapshot: window_ref.snapshot,
                    });
                }
            }
        } else if let Some(pid) = req.scope.pid {
            // 按 pid 匹配：收集候选，多于一个返回歧义。
            let candidates: Vec<String> = windows
                .iter()
                .filter(|w| w.get_process_id().unwrap_or(0) == pid as i32)
                .map(|w| w.get_name().unwrap_or_default())
                .filter(|n| !n.is_empty())
                .collect();
            if candidates.len() > 1 {
                return DesktopResult::Err(DesktopError::AmbiguousMatch { candidates });
            }
            match windows
                .into_iter()
                .find(|w| w.get_process_id().unwrap_or(0) == pid as i32)
            {
                Some(w) => w,
                None => {
                    return DesktopResult::Err(DesktopError::WindowNotFound {
                        query: format!("pid {pid}"),
                    });
                }
            }
        } else if let Some(name) = req.scope.app_name.as_deref() {
            // 按 app_name 匹配，收集候选，多于一个返回歧义。
            let needle = name.to_lowercase();
            let candidates: Vec<String> = windows
                .iter()
                .filter_map(|w| {
                    let title = w.get_name().unwrap_or_default();
                    title.to_lowercase().contains(&needle).then_some(title)
                })
                .collect();
            if candidates.len() > 1 {
                return DesktopResult::Err(DesktopError::AmbiguousMatch { candidates });
            }
            match windows.into_iter().find(|w| {
                w.get_name()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&needle)
            }) {
                Some(w) => w,
                None => {
                    return DesktopResult::Err(DesktopError::WindowNotFound {
                        query: name.to_string(),
                    });
                }
            }
        } else {
            return DesktopResult::Err(DesktopError::WindowNotFound {
                query: "snapshot 需要指定 window、pid 或 app_name".to_string(),
            });
        };
        let max_depth = if req.max_depth > 0 {
            req.max_depth
        } else {
            DEFAULT_MAX_DEPTH
        };
        let max_nodes = if req.max_nodes > 0 {
            req.max_nodes as usize
        } else {
            DEFAULT_MAX_NODES
        };
        let snapshot = self.next_snapshot();
        let mut nodes = Vec::new();
        let mut truncated = false;
        let warnings = Vec::new();
        Self::walk(
            &root,
            &automation,
            self,
            snapshot,
            0,
            max_depth,
            max_nodes,
            req.include_invisible,
            None,
            &mut nodes,
            &mut truncated,
        );
        // 先构建完成，再登记版本与提交节点表，避免并发残留。
        self.remember_snapshot(snapshot);
        self.snapshot_nodes
            .write()
            .unwrap()
            .insert(snapshot, nodes.clone());
        DesktopResult::Ok(SnapshotInfo {
            snapshot,
            nodes,
            truncated,
            warnings,
        })
    }

    async fn find(&self, req: &FindRequest) -> DesktopResult<FindInfo> {
        let (nodes, snapshot) = match req.snapshot {
            Some(s) => {
                let cached = self.snapshot_nodes.read().unwrap().get(&s).cloned();
                match cached {
                    Some(n) => (n, s),
                    None => return DesktopResult::Err(DesktopError::StaleElement { snapshot: s }),
                }
            }
            None => {
                // 未指定 snapshot 时需要 window 引用先取一次快照。
                let window = match &req.window {
                    Some(w) => w.clone(),
                    None => {
                        return DesktopResult::Err(DesktopError::ApplicationNotFound {
                            query: "find 需要指定 window 或 snapshot".to_string(),
                        });
                    }
                };
                let snap_req = SnapshotRequest {
                    scope: tiangong_plugin_computer_use_protocol::ops::SnapshotScope {
                        window: Some(window),
                        app_name: None,
                        pid: None,
                    },
                    max_depth: 0,
                    max_nodes: 0,
                    include_invisible: false,
                    access: Default::default(),
                };
                match self.snapshot(&snap_req).await {
                    DesktopResult::Ok(info) => (info.nodes, info.snapshot),
                    DesktopResult::Err(e) => return DesktopResult::Err(e),
                }
            }
        };
        let matches = filter_nodes(&nodes, &req.conditions);
        let ambiguous = matches.len() > 1;
        let max_candidates = if req.max_candidates > 0 {
            req.max_candidates as usize
        } else {
            matches.len()
        };
        let matches = matches.into_iter().take(max_candidates).collect();
        DesktopResult::Ok(FindInfo {
            matches,
            snapshot,
            ambiguous,
        })
    }

    async fn action(&self, req: &ActionRequest) -> DesktopResult<ActionResult> {
        // 从缓存取回 UIElement。
        let element = {
            let guard = self.elements.read().unwrap();
            guard
                .get(&(req.element.snapshot, req.element.id.clone()))
                .cloned()
        };
        let element = match element {
            Some(e) => e,
            None => {
                return DesktopResult::Err(DesktopError::StaleElement {
                    snapshot: req.element.snapshot,
                });
            }
        };
        // 动作前重新确认控件身份：从快照节点表取回原始属性，与当前控件比对。
        // 身份不一致说明界面已变化（控件被替换），返回 stale 避免误操作其他控件。
        let identity_ok = self
            .snapshot_nodes
            .read()
            .unwrap()
            .get(&req.element.snapshot)
            .and_then(|nodes| {
                nodes.iter().find(|n| {
                    n.element.id == req.element.id && n.element.snapshot == req.element.snapshot
                })
            })
            .is_some_and(|node| Self::verify_identity(&element, node));
        if !identity_ok {
            return DesktopResult::Err(DesktopError::StaleElement {
                snapshot: req.element.snapshot,
            });
        }
        // 动作前重新确认控件支持请求的动作。
        let action_kind = ActionKind::from(req.action);
        let supported = Self::detect_actions(&element);
        if !supported.contains(&action_kind) {
            return DesktopResult::Err(DesktopError::ActionNotSupported {
                action: format!("{action_kind:?}"),
                supported: supported.iter().map(|a| format!("{a:?}")).collect(),
            });
        }
        // 按 pattern 执行动作。
        let result = match action_kind {
            ActionKind::Focus => element.set_focus().map_err(|e| e.to_string()),
            ActionKind::Press => element
                .get_pattern::<UIInvokePattern>()
                .map_err(|e| e.to_string())
                .and_then(|p| p.invoke().map_err(|e| e.to_string())),
            ActionKind::SetValue => match &req.value {
                Some(v) => element
                    .get_pattern::<UIValuePattern>()
                    .map_err(|e| e.to_string())
                    .and_then(|p| p.set_value(v).map_err(|e| e.to_string())),
                None => Err("set_value 缺少 value 参数".to_string()),
            },
            ActionKind::Toggle => element
                .get_pattern::<UITogglePattern>()
                .map_err(|e| e.to_string())
                .and_then(|p| p.toggle().map_err(|e| e.to_string())),
            ActionKind::Select => element
                .get_pattern::<UISelectionItemPattern>()
                .map_err(|e| e.to_string())
                .and_then(|p| p.select().map_err(|e| e.to_string())),
            ActionKind::Expand => element
                .get_pattern::<UIExpandCollapsePattern>()
                .map_err(|e| e.to_string())
                .and_then(|p| p.expand().map_err(|e| e.to_string())),
            ActionKind::Collapse => element
                .get_pattern::<UIExpandCollapsePattern>()
                .map_err(|e| e.to_string())
                .and_then(|p| p.collapse().map_err(|e| e.to_string())),
            ActionKind::ScrollIntoView => element
                .get_pattern::<UIScrollItemPattern>()
                .map_err(|e| e.to_string())
                .and_then(|p| p.scroll_into_view().map_err(|e| e.to_string())),
        };
        match result {
            Ok(()) => DesktopResult::Ok(ActionResult {
                performed: true,
                summary: format!("已执行 {action_kind:?}"),
                new_window: None,
            }),
            Err(e) => DesktopResult::Err(DesktopError::BackendUnavailable { reason: e }),
        }
    }

    async fn wait(&self, req: &WaitRequest) -> DesktopResult<WaitResult> {
        match &req.condition {
            WaitCondition::Appear { target } | WaitCondition::Disappear { target } => {
                let looking_appear = matches!(req.condition, WaitCondition::Appear { .. });
                let deadline = Instant::now() + Duration::from_millis(req.timeout_ms);
                let start = Instant::now();
                loop {
                    let list_req = ListWindowsRequest::default();
                    let exists = match self.list_windows(&list_req).await {
                        DesktopResult::Ok(r) => r.windows.iter().any(|w| {
                            target.app_name.as_deref().is_some_and(|n| {
                                w.app_name.to_lowercase().contains(&n.to_lowercase())
                            }) || target
                                .title
                                .as_deref()
                                .is_some_and(|t| w.title.to_lowercase().contains(&t.to_lowercase()))
                        }),
                        DesktopResult::Err(e) => return DesktopResult::Err(e),
                    };
                    if looking_appear == exists {
                        return DesktopResult::Ok(WaitResult {
                            satisfied: true,
                            waited_ms: start.elapsed().as_millis() as u64,
                            matched_element: None,
                        });
                    }
                    if Instant::now() >= deadline {
                        return DesktopResult::Ok(WaitResult {
                            satisfied: false,
                            waited_ms: start.elapsed().as_millis() as u64,
                            matched_element: None,
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            _ => {
                // 控件级等待：从缓存取回 UIElement，轮询对应属性。
                let element = match &req.condition {
                    WaitCondition::Focus { element }
                    | WaitCondition::Available { element }
                    | WaitCondition::Value { element, .. } => element.clone(),
                    _ => unreachable!(),
                };
                let cached = self
                    .elements
                    .read()
                    .unwrap()
                    .get(&(element.snapshot, element.id.clone()))
                    .cloned();
                let uia_elem = match cached {
                    Some(e) => e,
                    None => {
                        return DesktopResult::Err(DesktopError::StaleElement {
                            snapshot: element.snapshot,
                        });
                    }
                };
                // 等待开始前重新确认控件仍属于原快照，避免缓存对象被替换后继续轮询。
                let identity_ok = self
                    .snapshot_nodes
                    .read()
                    .unwrap()
                    .get(&element.snapshot)
                    .and_then(|nodes| nodes.iter().find(|n| n.element == element))
                    .is_some_and(|node| Self::verify_identity(&uia_elem, node));
                if !identity_ok {
                    return DesktopResult::Err(DesktopError::StaleElement {
                        snapshot: element.snapshot,
                    });
                }
                let expected_value = match &req.condition {
                    WaitCondition::Value { expected, .. } => expected.clone(),
                    _ => None,
                };
                // 无期望值时必须成功读取初始值；否则无法判断后续是否真的发生变化。
                let initial_value = if expected_value.is_none()
                    && matches!(req.condition, WaitCondition::Value { .. })
                {
                    match uia_elem
                        .get_pattern::<UIValuePattern>()
                        .and_then(|p| p.get_value())
                    {
                        Ok(value) => Some(value),
                        Err(error) => {
                            return DesktopResult::Err(DesktopError::BackendUnavailable {
                                reason: format!("读取等待初始值失败: {error}"),
                            });
                        }
                    }
                } else {
                    None
                };
                let deadline = Instant::now() + Duration::from_millis(req.timeout_ms);
                let start = Instant::now();
                loop {
                    let state = match &req.condition {
                        WaitCondition::Focus { .. } => uia_elem.has_keyboard_focus(),
                        WaitCondition::Available { .. } => uia_elem.is_enabled(),
                        WaitCondition::Value { .. } => uia_elem
                            .get_pattern::<UIValuePattern>()
                            .and_then(|p| p.get_value())
                            .map(|current| match &expected_value {
                                Some(want) => current == *want,
                                None => initial_value.as_ref().is_some_and(|v| current != *v),
                            }),
                        _ => Ok(false),
                    };
                    let satisfied = match state {
                        Ok(value) => value,
                        Err(_) => {
                            return DesktopResult::Err(DesktopError::StaleElement {
                                snapshot: element.snapshot,
                            });
                        }
                    };
                    if satisfied {
                        return DesktopResult::Ok(WaitResult {
                            satisfied: true,
                            waited_ms: start.elapsed().as_millis() as u64,
                            matched_element: Some(element.clone()),
                        });
                    }
                    if Instant::now() >= deadline {
                        return DesktopResult::Ok(WaitResult {
                            satisfied: false,
                            waited_ms: start.elapsed().as_millis() as u64,
                            matched_element: None,
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }
}

impl WindowsBackend {
    /// 递归读取控件树（前序遍历），同时缓存 UIElement 供 action/wait 还原。
    #[allow(clippy::too_many_arguments)]
    fn walk(
        element: &UIElement,
        automation: &UIAutomation,
        backend: &WindowsBackend,
        snapshot: u64,
        depth: u32,
        max_depth: u32,
        max_nodes: usize,
        _include_invisible: bool,
        parent_id: Option<String>,
        nodes: &mut Vec<ControlNode>,
        truncated: &mut bool,
    ) {
        if nodes.len() >= max_nodes || depth > max_depth {
            *truncated = true;
            return;
        }
        let control_type = element.get_control_type().unwrap_or(ControlType::Custom);
        let role = format!("{control_type:?}");
        let name = element.get_name().unwrap_or_default();
        let automation_id = element.get_automation_id().ok();
        let enabled = element.is_enabled().unwrap_or(true);
        let focused = element.has_keyboard_focus().unwrap_or(false);
        let bounds = Self::bounds_of(element);
        let actions = Self::detect_actions(element);
        // 读取控件当前值：密码控件不读取（敏感），非敏感且支持 Value Pattern 时读取。
        let sensitive = element.is_password().unwrap_or(false);
        let value = if sensitive {
            None
        } else {
            element
                .get_pattern::<UIValuePattern>()
                .ok()
                .and_then(|p| p.get_value().ok())
        };

        let id = format!("uia-{snapshot}-{}", nodes.len());
        let parent_index = nodes.len();
        // 缓存 UIElement 供 action/wait 还原。
        backend
            .elements
            .write()
            .unwrap()
            .insert((snapshot, id.clone()), element.clone());
        nodes.push(ControlNode {
            element: ElementRef {
                id: id.clone(),
                snapshot,
            },
            role: role.clone(),
            name,
            identifiers: StableIdentifiers {
                automation_id,
                role: Some(role),
            },
            value,
            sensitive,
            visible: true,
            enabled,
            focused,
            bounds,
            actions,
            parent: parent_id.map(|pid| ElementRef { id: pid, snapshot }),
            children: Vec::new(),
        });

        // 读取子元素。
        let true_cond = match automation.create_true_condition() {
            Ok(c) => c,
            Err(_) => return,
        };
        let children = match element.find_all(TreeScope::Children, &true_cond) {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut child_refs = Vec::with_capacity(children.len());
        for child in children {
            let before = nodes.len();
            Self::walk(
                &child,
                automation,
                backend,
                snapshot,
                depth + 1,
                max_depth,
                max_nodes,
                _include_invisible,
                Some(id.clone()),
                nodes,
                truncated,
            );
            if nodes.len() > before {
                child_refs.push(nodes[before].element.clone());
            }
        }
        if let Some(node) = nodes.get_mut(parent_index) {
            node.children = child_refs;
        }
    }
}

/// 按条件筛选节点（平台无关逻辑）。
fn filter_nodes(nodes: &[ControlNode], conditions: &FindConditions) -> Vec<ControlNode> {
    nodes
        .iter()
        .filter(|n| {
            let id_match = conditions
                .automation_id
                .as_ref()
                .map(|want| {
                    n.identifiers
                        .automation_id
                        .as_deref()
                        .is_some_and(|have| have == want)
                })
                .unwrap_or(true);
            let role_match = conditions
                .role
                .as_ref()
                .map(|want| {
                    n.identifiers
                        .role
                        .as_deref()
                        .is_some_and(|have| have == want)
                })
                .unwrap_or(true);
            let name_match = conditions.name.as_ref().map(|want| match conditions.mode {
                MatchMode::Exact => n.name == *want,
                MatchMode::Contains => n.name.to_lowercase().contains(&want.to_lowercase()),
            });
            let value_match = conditions.value.as_ref().map(|want| {
                n.value
                    .as_deref()
                    .is_some_and(|have| match conditions.mode {
                        MatchMode::Exact => have == want,
                        MatchMode::Contains => have.to_lowercase().contains(&want.to_lowercase()),
                    })
            });
            let visible_match = conditions
                .visible
                .map(|want| n.visible == want)
                .unwrap_or(true);
            let enabled_match = conditions
                .enabled
                .map(|want| n.enabled == want)
                .unwrap_or(true);
            let focused_match = conditions
                .focused
                .map(|want| n.focused == want)
                .unwrap_or(true);
            // 敏感控件不进入查找结果。
            let not_sensitive = !n.sensitive;
            id_match
                && role_match
                && name_match.unwrap_or(true)
                && value_match.unwrap_or(true)
                && visible_match
                && enabled_match
                && focused_match
                && not_sensitive
        })
        .cloned()
        .collect()
}

/// 按应用名（窗口标题包含匹配）从顶层窗口中查找进程号。
fn find_window_pid_by_name(automation: &UIAutomation, name: &str) -> Option<u32> {
    let windows = WindowsBackend::top_level_windows(automation).ok()?;
    let needle = name.to_lowercase();
    windows.into_iter().find_map(|w| {
        let title = w.get_name().unwrap_or_default();
        if title.to_lowercase().contains(&needle) {
            w.get_process_id().ok().map(|p| p as u32)
        } else {
            None
        }
    })
}
