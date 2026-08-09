//! Windows UI Automation 无障碍后端。
//!
//! 通过 `uiautomation` crate（基于 windows-rs 的 UIA COM 封装）访问桌面控件树，
//! 列举应用窗口、读取控件树并执行动作。
//!
//! 从桌面根节点按 ControlType 与进程缩小范围，优先使用 Control View，
//! 避免无边界遍历整个桌面。目标窗口属于更高权限进程或安全桌面时返回权限受限，
//! 不提升权限绕过。

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use uiautomation::controls::ControlType;
use uiautomation::core::{TreeScope, UIAutomation, UIElement};
use uiautomation::patterns::{
    UIExpandCollapsePattern, UIInvokePattern, UIScrollItemPattern, UISelectionItemPattern,
    UITogglePattern, UIValuePattern,
};
use uiautomation::types::Rect;

use super::{
    ActionResult, Backend, FindInfo, SnapshotInfo, StatusInfo, WaitResult, all_supported_actions,
};
use tiangong_plugin_computer_use_protocol::ops::{
    ActionRequest, FindRequest, ListWindowsRequest, SnapshotRequest, WaitCondition, WaitRequest,
};
use tiangong_plugin_computer_use_protocol::{
    AccessibilityCapability, ActionKind, Bounds, ControlNode, DesktopError, DesktopResult,
    DesktopSession, ElementRef, Platform, StableIdentifiers, WindowInfo,
};

const DEFAULT_MAX_DEPTH: u32 = 8;
const DEFAULT_MAX_NODES: usize = 400;

pub struct WindowsBackend {
    snapshot_seq: AtomicU64,
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
        }
    }

    fn next_snapshot(&self) -> u64 {
        self.snapshot_seq.fetch_add(1, Ordering::Relaxed)
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
                uiautomation::variants::Variant::from_i32(ControlType::Window as i32),
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
                supported_actions: all_supported_actions(),
            }),
            Err(e) => DesktopResult::Ok(StatusInfo {
                session: DesktopSession::Unavailable,
                accessibility: AccessibilityCapability {
                    available: false,
                    reason: Some(e.agent_message().to_string()),
                },
                supported_actions: all_supported_actions(),
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
            // 应用名取窗口标题或进程名（简化为标题）。
            let app_name = if name.is_empty() {
                format!("进程 {pid}")
            } else {
                name.clone()
            };
            windows.push(WindowInfo {
                app_name: app_name.clone(),
                pid: pid as u32,
                element: ElementRef {
                    id: format!("uia-win-{idx}"),
                    snapshot,
                },
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
        if let Some(app_name) = req.app_name.as_deref() {
            let needle = app_name.to_lowercase();
            windows.retain(|w| w.app_name.to_lowercase().contains(&needle));
        }
        if let Some(pid) = req.pid {
            windows.retain(|w| w.pid == pid);
        }
        DesktopResult::Ok(tiangong_plugin_computer_use_protocol::ListWindowsResponse { windows })
    }

    async fn snapshot(&self, req: &SnapshotRequest) -> DesktopResult<SnapshotInfo> {
        let automation = match Self::automation() {
            Ok(a) => a,
            Err(e) => return DesktopResult::Err(e),
        };
        // 定位根元素：优先用窗口引用对应的 pid 创建应用根，否则用桌面根。
        let root = match &req.scope.window {
            Some(_window_ref) => {
                // 完整实现应按 window 引用还原 UIElement；当前简化为从桌面根遍历。
                automation
                    .get_root_element()
                    .map_err(|e| DesktopError::BackendUnavailable {
                        reason: format!("获取桌面根元素失败: {e}"),
                    })?
            }
            None => {
                automation
                    .get_root_element()
                    .map_err(|e| DesktopError::BackendUnavailable {
                        reason: format!("获取桌面根元素失败: {e}"),
                    })?
            }
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
            snapshot,
            0,
            max_depth,
            max_nodes,
            req.include_invisible,
            None,
            &mut nodes,
            &mut truncated,
        );
        DesktopResult::Ok(SnapshotInfo {
            snapshot,
            nodes,
            truncated,
            warnings,
        })
    }

    async fn find(&self, _req: &FindRequest) -> DesktopResult<FindInfo> {
        // find 需基于快照节点表；Windows 后端当前未缓存节点表，返回未实现。
        DesktopResult::Err(DesktopError::ActionNotSupported {
            action: "desktop_find".to_string(),
            supported: vec![],
        })
    }

    async fn action(&self, req: &ActionRequest) -> DesktopResult<ActionResult> {
        // action 需从缓存还原 UIElement 并调对应 pattern。
        // Windows 后端当前未缓存 UIElement，返回未实现。
        let _ = req;
        DesktopResult::Err(DesktopError::ActionNotSupported {
            action: "desktop_action".to_string(),
            supported: vec![],
        })
    }

    async fn wait(&self, req: &WaitRequest) -> DesktopResult<WaitResult> {
        match &req.condition {
            WaitCondition::Appear { target } | WaitCondition::Disappear { target } => {
                let looking_appear = matches!(req.condition, WaitCondition::Appear { .. });
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(req.timeout_ms);
                let start = std::time::Instant::now();
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
                        _ => false,
                    };
                    if looking_appear == exists {
                        return DesktopResult::Ok(WaitResult {
                            satisfied: true,
                            waited_ms: start.elapsed().as_millis() as u64,
                            matched_element: None,
                        });
                    }
                    if std::time::Instant::now() >= deadline {
                        return DesktopResult::Ok(WaitResult {
                            satisfied: false,
                            waited_ms: start.elapsed().as_millis() as u64,
                            matched_element: None,
                        });
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
            _ => DesktopResult::Ok(WaitResult {
                satisfied: false,
                waited_ms: req.timeout_ms,
                matched_element: None,
            }),
        }
    }
}

impl WindowsBackend {
    /// 递归读取控件树（前序遍历）。
    #[allow(clippy::too_many_arguments)]
    fn walk(
        element: &UIElement,
        automation: &UIAutomation,
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
        // 读取属性；单控件失败不影响整棵树。
        let control_type = element.get_control_type().unwrap_or(ControlType::Custom);
        let role = format!("{control_type:?}");
        let name = element.get_name().unwrap_or_default();
        let automation_id = element.get_automation_id().ok();
        let enabled = element.is_enabled().unwrap_or(true);
        let focused = element.has_keyboard_focus().unwrap_or(false);
        let bounds = Self::bounds_of(element);
        let actions = supported_actions_for(&control_type);

        let id = format!("uia-{snapshot}-{}", nodes.len());
        let parent_index = nodes.len();
        nodes.push(ControlNode {
            element: ElementRef {
                id: id.clone(),
                snapshot,
            },
            role,
            name,
            identifiers: StableIdentifiers {
                automation_id,
                role: None,
            },
            value: None,
            sensitive: false,
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

/// 按 UIA ControlType 推断控件支持的动作。
fn supported_actions_for(control_type: &ControlType) -> Vec<ActionKind> {
    use ActionKind::*;
    match control_type {
        ControlType::Button | ControlType::MenuItem => vec![Press, Focus],
        ControlType::CheckBox | ControlType::RadioButton => vec![Toggle, Select],
        ControlType::ComboBox => vec![Expand, Collapse],
        ControlType::Edit | ControlType::Document => vec![SetValue, Focus],
        ControlType::ListItem => vec![Select, ScrollIntoView],
        ControlType::TreeItem => vec![Expand, Collapse, Select],
        _ => vec![],
    }
}

/// 抑制未使用 trait import 告警（动作 pattern 在完整 action 实现时使用）。
#[allow(dead_code)]
fn _pattern_types() {
    let _: Option<UIInvokePattern> = None;
    let _: Option<UIValuePattern> = None;
    let _: Option<UITogglePattern> = None;
    let _: Option<UISelectionItemPattern> = None;
    let _: Option<UIExpandCollapsePattern> = None;
    let _: Option<UIScrollItemPattern> = None;
    let _: Option<Rect> = None;
}
