//! macOS 无障碍后端。
//!
//! 通过 `objc2-app-kit` 的 `NSWorkspace` 列举运行中的应用，作为窗口发现的基础；
//! 通过 ApplicationServices 的 AXUIElement C API 读取控件树、查找控件并执行动作。
//! 辅助功能授权经 `AXIsProcessTrusted` 探测。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use objc2_app_kit::NSWorkspace;

use super::ax::{self, AxElement};
use super::{ActionResult, Backend, FindInfo, SnapshotInfo, StatusInfo, WaitResult};
use tiangong_plugin_computer_use_protocol::{
    AccessibilityCapability, ActionKind, Bounds, ControlNode, DesktopError, DesktopResult,
    DesktopSession, ElementRef, Platform, StableIdentifiers, WindowInfo,
};

/// 默认最大遍历深度。
const DEFAULT_MAX_DEPTH: u32 = 8;
/// 默认最大节点数。
const DEFAULT_MAX_NODES: usize = 400;

pub struct MacosBackend {
    snapshot_seq: AtomicU64,
    /// 快照内控件引用缓存：((snapshot, id) -> AxElement)。
    /// 仅保留最近若干个快照，避免内存无限增长。
    elements: RwLock<HashMap<(u64, String), AxElement>>,
    /// 快照节点表缓存：(snapshot -> Vec<ControlNode>)，供 find 在指定快照内筛选。
    snapshot_nodes: RwLock<HashMap<u64, Vec<ControlNode>>>,
    /// 已使用的快照版本，用于清理过期缓存。
    recent_snapshots: Mutex<Vec<u64>>,
}

impl Default for MacosBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MacosBackend {
    pub fn new() -> Self {
        Self {
            snapshot_seq: AtomicU64::new(1),
            elements: RwLock::new(HashMap::new()),
            snapshot_nodes: RwLock::new(HashMap::new()),
            recent_snapshots: Mutex::new(Vec::new()),
        }
    }

    /// 探测辅助功能授权是否已授予。
    fn is_trusted() -> bool {
        ax::is_process_trusted()
    }

    /// 下一个快照版本号。
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
            let mut elements = self.elements.write().unwrap();
            elements.retain(|(s, _), _| *s != old);
            self.snapshot_nodes.write().unwrap().remove(&old);
        }
    }

    /// 缓存控件引用，返回分配的 id。
    fn cache_element(&self, snapshot: u64, element: AxElement) -> String {
        let id = format!("ax-{}-{}", snapshot, element.raw() as usize);
        self.elements
            .write()
            .unwrap()
            .insert((snapshot, id.clone()), element);
        id
    }

    /// 列举运行中的应用，构造窗口候选列表。
    fn enumerate_windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = workspace.runningApplications();
        let snapshot = self.next_snapshot();
        let mut windows = Vec::new();
        let mut idx = 0u64;
        for app in apps.iter() {
            let active = app.isActive();
            let name: String = app
                .localizedName()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let pid = app.processIdentifier();
            // element ref 用基于 pid 的确定性 id（格式 macos-app-{pid}-{idx}）。
            // 不在此缓存 AxElement：list_windows 可能在 wait 轮询中高频调用，
            // 缓存会导致原生对象累积。snapshot 时通过 parse_pid_from_id 重新创建应用根。
            let id = format!("macos-app-{pid}-{idx}");
            idx += 1;
            windows.push(WindowInfo {
                app_name: name.clone(),
                pid: pid as u32,
                element: ElementRef { id, snapshot },
                title: name.clone(),
                is_foreground: active,
                bounds: Bounds::default(),
                visible: true,
                enabled: true,
                identifiers: StableIdentifiers {
                    automation_id: None,
                    role: Some("AXApplication".to_string()),
                },
            });
        }
        Ok(windows)
    }

    /// 递归读取控件树，填充节点列表与引用缓存。
    fn build_snapshot(
        &self,
        root: AxElement,
        snapshot: u64,
        max_depth: u32,
        max_nodes: usize,
        include_invisible: bool,
    ) -> (Vec<ControlNode>, bool, Vec<String>) {
        let mut nodes = Vec::new();
        let mut truncated = false;
        let mut warnings = Vec::new();
        self.walk(
            root,
            snapshot,
            0,
            max_depth,
            max_nodes,
            include_invisible,
            None,
            &mut nodes,
            &mut truncated,
            &mut warnings,
        );
        (nodes, truncated, warnings)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        element: AxElement,
        snapshot: u64,
        depth: u32,
        max_depth: u32,
        max_nodes: usize,
        include_invisible: bool,
        parent_id: Option<String>,
        nodes: &mut Vec<ControlNode>,
        truncated: &mut bool,
        warnings: &mut Vec<String>,
    ) {
        if nodes.len() >= max_nodes {
            *truncated = true;
            return;
        }
        if depth > max_depth {
            *truncated = true;
            return;
        }

        // 先读子控件（返回独立的 owned Vec），再读属性，最后 cache element。
        // children() 返回的 AxElement 与父元素相互独立，不共享所有权。
        // AttributeUnsupported 是正常的（叶子控件无子节点），静默处理；
        // 其余错误（如 CannotComplete）才记入告警，避免整棵树失败。
        let children = match element.children() {
            Ok(cs) => cs,
            Err(crate::backend::ax::AxError::AttributeUnsupported) => Vec::new(),
            Err(_) => {
                warnings.push(format!("读取子控件失败（深度 {depth}）"));
                Vec::new()
            }
        };

        let role = element.string_attribute("AXRole").unwrap_or_default();
        let title = element.string_attribute("AXTitle").unwrap_or_default();
        let identifier = element.string_attribute("AXIdentifier");
        let enabled = element.bool_attribute("AXEnabled").unwrap_or(true);
        let focused = element.bool_attribute("AXFocused").unwrap_or(false);
        let value = element.string_attribute("AXValue");
        // 读取控件边界（AXPosition + AXSize）；失败返回默认零值，不影响整棵树。
        let ax_bounds = element.bounds();
        // 敏感控件（密码框）的值不返回。
        let sensitive = matches!(role.as_str(), "AXSecureTextField");
        let safe_value = if sensitive { None } else { value };
        let visible = true; // 完整可见性需读 AXChildren 可见性，此处放宽。
        if !include_invisible && !visible {
            return;
        }

        // 缓存控件引用（消费 element），递归时按 children 独立处理。
        let id = self.cache_element(snapshot, element);
        let actions = supported_actions_for(&role);
        let bounds = Bounds {
            x: ax_bounds.x,
            y: ax_bounds.y,
            width: ax_bounds.width,
            height: ax_bounds.height,
        };

        // 前序遍历：先 push 父节点（children 暂空），记录索引后递归子节点，
        // 递归结束回填 children，保证输出顺序为“父→子→孙”。
        let parent_index = nodes.len();
        nodes.push(ControlNode {
            element: ElementRef {
                id: id.clone(),
                snapshot,
            },
            role: role.clone(),
            name: title,
            identifiers: StableIdentifiers {
                automation_id: identifier,
                role: Some(role),
            },
            value: safe_value,
            sensitive,
            visible,
            enabled,
            focused,
            bounds,
            actions,
            parent: parent_id.map(|pid| ElementRef { id: pid, snapshot }),
            children: Vec::new(),
        });

        // 递归子节点，收集其 element 引用并回填到父节点 children。
        let mut child_refs = Vec::with_capacity(children.len());
        for child in children {
            let child_index_before = nodes.len();
            self.walk(
                child,
                snapshot,
                depth + 1,
                max_depth,
                max_nodes,
                include_invisible,
                Some(id.clone()),
                nodes,
                truncated,
                warnings,
            );
            if nodes.len() > child_index_before {
                child_refs.push(nodes[child_index_before].element.clone());
            }
        }
        // 回填父节点的 children（父节点一定还在，因为子节点递归只往后追加）。
        if let Some(node) = nodes.get_mut(parent_index) {
            node.children = child_refs;
        }
    }
}

/// 按角色推断控件支持的动作（macOS AX 动作名映射）。
fn supported_actions_for(role: &str) -> Vec<ActionKind> {
    use ActionKind::*;
    match role {
        "AXButton" | "AXPopUpButton" | "AXCheckBox" | "AXMenuButton" => vec![Press],
        "AXTextField" | "AXTextArea" | "AXComboBox" => vec![SetValue, Focus],
        "AXSecureTextField" => vec![],
        "AXSlider" | "AXIncrementor" => vec![SetValue],
        "AXRadioButton" => vec![Select],
        "AXOutline" | "AXRow" | "AXCell" => vec![Select],
        // AXDisclosureTriangle/AXPopOver 的展开折叠在 AX 中无语义动作（AXPress 是
        // 切换，无法保证方向），暂不声明，避免误导。
        _ => vec![],
    }
}

/// AX 动作名常量。
const AX_PRESS: &str = "AXPress";
const AX_SET_VALUE: &str = "AXValue";
const AX_FOCUSED: &str = "AXFocused";

#[async_trait]
impl Backend for MacosBackend {
    fn platform(&self) -> Platform {
        Platform::Macos
    }

    async fn status(&self) -> DesktopResult<StatusInfo> {
        let trusted = Self::is_trusted();
        let (session, accessibility) = if trusted {
            (
                DesktopSession::Available,
                AccessibilityCapability {
                    available: true,
                    reason: None,
                },
            )
        } else {
            (
                DesktopSession::NotReady,
                AccessibilityCapability {
                    available: false,
                    reason: Some("尚未授予辅助功能权限".to_string()),
                },
            )
        };
        DesktopResult::Ok(StatusInfo {
            session,
            accessibility,
            supported_actions: super::macos_supported_actions(),
        })
    }

    async fn list_windows(
        &self,
        req: &tiangong_plugin_computer_use_protocol::ops::ListWindowsRequest,
    ) -> DesktopResult<tiangong_plugin_computer_use_protocol::ListWindowsResponse> {
        // NSWorkspace 列举应用不需要辅助功能授权；只有读取窗口 AX 属性才需要。
        // 因此 list_windows 在未授权时也能返回应用基础信息，便于 Agent 了解当前桌面。
        let mut windows = match self.enumerate_windows() {
            Ok(w) => w,
            Err(e) => return DesktopResult::Err(e),
        };
        if let Some(app_name) = req.app_name.as_deref() {
            let needle = app_name.to_lowercase();
            windows.retain(|w| w.app_name.to_lowercase().contains(&needle));
        }
        if let Some(pid) = req.pid {
            windows.retain(|w| w.pid == pid);
        }
        if req.foreground_only {
            windows.retain(|w| w.is_foreground);
        }
        DesktopResult::Ok(tiangong_plugin_computer_use_protocol::ListWindowsResponse { windows })
    }

    async fn snapshot(
        &self,
        req: &tiangong_plugin_computer_use_protocol::ops::SnapshotRequest,
    ) -> DesktopResult<SnapshotInfo> {
        if !Self::is_trusted() {
            return DesktopResult::Err(DesktopError::PermissionDenied {
                reason: "尚未授予辅助功能权限".to_string(),
            });
        }
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

        // 定位根元素：优先用窗口引用（从 id 解析 pid），其次按应用名/pid 创建。
        let root = match &req.scope.window {
            Some(window_ref) => match parse_pid_from_id(&window_ref.id) {
                Some(pid) => AxElement::for_application(pid),
                None => {
                    return DesktopResult::Err(DesktopError::WindowNotFound {
                        query: window_ref.id.clone(),
                    });
                }
            },
            None => {
                // 按应用名匹配时收集所有候选，多于一个返回歧义错误，不静默选第一个。
                let pid = match req.scope.pid {
                    Some(pid) => Some(pid),
                    None => match &req.scope.app_name {
                        Some(name) => {
                            let matches = find_app_pids_by_name(name);
                            if matches.len() > 1 {
                                return DesktopResult::Err(DesktopError::AmbiguousMatch {
                                    candidates: vec![format!(
                                        "{name}（{} 个匹配进程）",
                                        matches.len()
                                    )],
                                });
                            }
                            matches.first().copied()
                        }
                        None => None,
                    },
                };
                match pid {
                    Some(pid) => AxElement::for_application(pid as i32),
                    None => {
                        return DesktopResult::Err(DesktopError::ApplicationNotFound {
                            query: req
                                .scope
                                .app_name
                                .clone()
                                .unwrap_or_else(|| "未指定目标".to_string()),
                        });
                    }
                }
            }
        };

        // 先分配版本号并构建节点，完成后再登记版本与写节点表。
        // 避免先登记导致并发请求在慢快照构建期间将其淘汰，而慢请求随后仍写回残留缓存。
        let snapshot = self.next_snapshot();
        let (nodes, truncated, warnings) =
            self.build_snapshot(root, snapshot, max_depth, max_nodes, req.include_invisible);
        // 构建完成后一次性登记并提交节点表；walk 写入的 elements 也以此版本号为准。
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

    async fn find(
        &self,
        req: &tiangong_plugin_computer_use_protocol::ops::FindRequest,
    ) -> DesktopResult<FindInfo> {
        // find 基于 snapshot：若指定 snapshot 版本，从缓存的节点表筛选；
        // 否则需要 window 引用先取一次快照。
        let (nodes, snapshot) = match req.snapshot {
            Some(s) => {
                // 从缓存的节点表读取；快照已过期则报 stale。
                let cached = self.snapshot_nodes.read().unwrap().get(&s).cloned();
                match cached {
                    Some(n) => (n, s),
                    None => {
                        return DesktopResult::Err(DesktopError::StaleElement { snapshot: s });
                    }
                }
            }
            None => {
                let window = match &req.window {
                    Some(w) => w.clone(),
                    None => {
                        return DesktopResult::Err(DesktopError::ApplicationNotFound {
                            query: "find 需要指定 window 或 snapshot".to_string(),
                        });
                    }
                };
                let snap_req = tiangong_plugin_computer_use_protocol::ops::SnapshotRequest {
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
                let info = match self.snapshot(&snap_req).await {
                    DesktopResult::Ok(i) => i,
                    DesktopResult::Err(e) => return DesktopResult::Err(e),
                };
                (info.nodes, info.snapshot)
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

    async fn action(
        &self,
        req: &tiangong_plugin_computer_use_protocol::ops::ActionRequest,
    ) -> DesktopResult<ActionResult> {
        if !Self::is_trusted() {
            return DesktopResult::Err(DesktopError::PermissionDenied {
                reason: "尚未授予辅助功能权限".to_string(),
            });
        }
        // 从缓存取回真实控件。
        let element = {
            let guard = self.elements.read().unwrap();
            guard
                .get(&(req.element.snapshot, req.element.id.clone()))
                .map(|_| req.element.id.clone())
        };
        let element_id = match element {
            Some(id) => id,
            None => {
                return DesktopResult::Err(DesktopError::StaleElement {
                    snapshot: req.element.snapshot,
                });
            }
        };

        // 重新创建控件对象：从缓存取引用。
        let element_ref = {
            let mut guard = self.elements.write().unwrap();
            guard
                .remove(&(req.element.snapshot, element_id.clone()))
                .ok_or(DesktopError::StaleElement {
                    snapshot: req.element.snapshot,
                })
        };
        let element = match element_ref {
            Ok(e) => e,
            Err(e) => return DesktopResult::Err(e),
        };

        let action_kind = ActionKind::from(req.action);
        let result = match action_kind {
            ActionKind::Focus => element.set_bool_attribute(AX_FOCUSED, true),
            ActionKind::Press => element.perform_action(AX_PRESS),
            ActionKind::SetValue => match &req.value {
                Some(v) => element.set_string_attribute(AX_SET_VALUE, v),
                None => Err(ax::AxError::IllegalArgument),
            },
            ActionKind::Toggle => element.perform_action(AX_PRESS),
            ActionKind::Select => element.perform_action(AX_PRESS),
            // Expand/Collapse 在 AX 中无法按语义区分（AXPress 是切换，无法保证
            // 折叠已折叠控件不变成展开）；ScrollIntoView 无对应 AX 动作（AXRaise
            // 是提升窗口而非滚动控件）。无法保证语义时返回不支持，而非错误执行。
            ActionKind::Expand | ActionKind::Collapse | ActionKind::ScrollIntoView => {
                Err(ax::AxError::ActionUnsupported)
            }
        };

        // 重新缓存控件（动作后控件仍可用）。
        self.elements
            .write()
            .unwrap()
            .insert((req.element.snapshot, element_id.clone()), element);

        match result {
            Ok(()) => DesktopResult::Ok(ActionResult {
                performed: true,
                summary: format!("已执行 {:?}", action_kind),
                new_window: None,
            }),
            Err(e) => match e {
                ax::AxError::ActionUnsupported => {
                    DesktopResult::Err(DesktopError::ActionNotSupported {
                        action: format!("{:?}", action_kind),
                        supported: vec![],
                    })
                }
                ax::AxError::InvalidUIElement => DesktopResult::Err(DesktopError::StaleElement {
                    snapshot: req.element.snapshot,
                }),
                ax::AxError::CannotComplete => DesktopResult::Err(DesktopError::PermissionDenied {
                    reason: e.message().to_string(),
                }),
                _ => DesktopResult::Err(DesktopError::BackendUnavailable {
                    reason: e.message().to_string(),
                }),
            },
        }
    }

    async fn wait(
        &self,
        req: &tiangong_plugin_computer_use_protocol::ops::WaitRequest,
    ) -> DesktopResult<WaitResult> {
        let deadline = Instant::now() + Duration::from_millis(req.timeout_ms);
        use tiangong_plugin_computer_use_protocol::ops::WaitCondition;
        match &req.condition {
            WaitCondition::Appear { target } | WaitCondition::Disappear { target } => {
                let looking_appear = matches!(req.condition, WaitCondition::Appear { .. });
                let start = Instant::now();
                loop {
                    let exists = self.target_exists(target);
                    if looking_appear && exists {
                        return DesktopResult::Ok(WaitResult {
                            satisfied: true,
                            waited_ms: start.elapsed().as_millis() as u64,
                            matched_element: None,
                        });
                    }
                    if !looking_appear && !exists {
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
            WaitCondition::Focus { element }
            | WaitCondition::Available { element }
            | WaitCondition::Value { element, .. } => {
                // 控件级等待：从缓存取回控件引用，轮询对应属性直到满足或超时。
                let expected_value = match &req.condition {
                    WaitCondition::Value { expected, .. } => expected.clone(),
                    _ => None,
                };
                let start = Instant::now();
                let cached = self
                    .elements
                    .read()
                    .unwrap()
                    .get(&(element.snapshot, element.id.clone()))
                    .cloned();
                let ax_element = match cached {
                    Some(e) => e,
                    None => {
                        return DesktopResult::Err(DesktopError::StaleElement {
                            snapshot: element.snapshot,
                        });
                    }
                };
                // 记录初始值：value 等待无期望值时，需等到值真正发生变化才算成功。
                let initial_value = if expected_value.is_none()
                    && matches!(&req.condition, WaitCondition::Value { .. })
                {
                    ax_element.string_attribute("AXValue")
                } else {
                    None
                };
                loop {
                    let satisfied = match &req.condition {
                        WaitCondition::Focus { .. } => {
                            ax_element.bool_attribute("AXFocused").unwrap_or(false)
                        }
                        WaitCondition::Available { .. } => {
                            ax_element.bool_attribute("AXEnabled").unwrap_or(false)
                        }
                        WaitCondition::Value { .. } => match &expected_value {
                            Some(want) => ax_element
                                .string_attribute("AXValue")
                                .is_some_and(|v| v == *want),
                            None => {
                                // 无期望值：比较当前值与初始值，不同才算变化。
                                let current = ax_element.string_attribute("AXValue");
                                current != initial_value
                            }
                        },
                        // Appear/Disappear 已在外层处理，这里不会到达。
                        _ => false,
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

impl MacosBackend {
    fn target_exists(
        &self,
        target: &tiangong_plugin_computer_use_protocol::ops::WaitTarget,
    ) -> bool {
        let Ok(windows) = self.enumerate_windows() else {
            return false;
        };
        windows.iter().any(|w| {
            target
                .app_name
                .as_deref()
                .is_some_and(|n| w.app_name.to_lowercase().contains(&n.to_lowercase()))
                || target
                    .title
                    .as_deref()
                    .is_some_and(|t| w.title.to_lowercase().contains(&t.to_lowercase()))
        })
    }
}

/// 从 element id（格式 macos-app-{pid}-{idx}）解析 pid。
fn parse_pid_from_id(id: &str) -> Option<i32> {
    let parts: Vec<&str> = id.split('-').collect();
    if parts.len() >= 3 && parts[0] == "macos" && parts[1] == "app" {
        parts[2].parse().ok()
    } else {
        None
    }
}

/// 按应用名查找所有匹配的进程号（包含匹配），供调用方判断歧义。
fn find_app_pids_by_name(name: &str) -> Vec<u32> {
    let workspace = NSWorkspace::sharedWorkspace();
    let apps = workspace.runningApplications();
    let needle = name.to_lowercase();
    apps.iter()
        .filter_map(|app| {
            let app_name: String = app
                .localizedName()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if app_name.to_lowercase().contains(&needle) {
                Some(app.processIdentifier() as u32)
            } else {
                None
            }
        })
        .collect()
}

/// 按条件筛选节点。
fn filter_nodes(
    nodes: &[ControlNode],
    conditions: &tiangong_plugin_computer_use_protocol::ops::FindConditions,
) -> Vec<ControlNode> {
    use tiangong_plugin_computer_use_protocol::MatchMode;
    nodes
        .iter()
        .filter(|n| {
            // 稳定标识 + 类型 优先，名称补充。
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
            let name_match = conditions.name.as_ref().map(|want| {
                let have = n.name.as_str();
                match conditions.mode {
                    MatchMode::Exact => have == want,
                    MatchMode::Contains => have.to_lowercase().contains(&want.to_lowercase()),
                }
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
            id_match
                && role_match
                && name_match.unwrap_or(true)
                && value_match.unwrap_or(true)
                && visible_match
                && enabled_match
                && focused_match
        })
        .cloned()
        .collect()
}
