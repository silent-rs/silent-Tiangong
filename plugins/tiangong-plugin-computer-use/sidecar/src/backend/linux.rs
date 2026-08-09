//! Linux AT-SPI2 无障碍后端。
//!
//! 通过 `atspi` crate（纯 Rust、基于 zbus/D-Bus 的 AT-SPI2 实现）访问桌面会话的
//! accessibility bus，列举应用、读取控件树并执行动作。
//!
//! 适用 GTK、Qt、Electron 等正常暴露 AT-SPI 信息的应用。Wayland 下仍以 AT-SPI
//! 语义动作为主，不依赖 xdotool/wmctrl 等 X11 坐标工具。
//!
//! 无 accessibility bus（纯 SSH/容器/无头环境）或受 Flatpak/Snap 策略限制时，
//! 返回 `desktop_session_unavailable` 及原因，不影响宿主启动。

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use atspi::{
    AccessibilityConnection, Interface, InterfaceSet, Role, State,
    proxy::accessible::{AccessibleProxy, ObjectRefExt},
};

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
/// 根桌面对象路径与 AT-SPI 注册表的 well-known 服务名。
/// 注意：`:1.x` 是动态 D-Bus unique name，会随服务启动顺序变化，不可使用；
/// 必须用 well-known name `org.a11y.atspi.Registry`，zbus 会解析到当前注册表实例。
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const ROOT_DEST: &str = "org.a11y.atspi.Registry";

pub struct LinuxBackend {
    snapshot_seq: AtomicU64,
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxBackend {
    pub fn new() -> Self {
        Self {
            snapshot_seq: AtomicU64::new(1),
        }
    }

    fn next_snapshot(&self) -> u64 {
        self.snapshot_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// 尝试连接 accessibility bus。连接失败说明无图形会话或未启用无障碍。
    /// 加 5 秒超时，防止无 D-Bus 环境下连接调用阻塞。
    async fn connect(&self) -> Result<AccessibilityConnection, DesktopError> {
        let connect = async {
            if let Err(e) = atspi::connection::set_session_accessibility(true).await {
                tracing::debug!(error = %e, "启用会话无障碍失败，继续尝试连接");
            }
            AccessibilityConnection::new().await
        };
        match tokio::time::timeout(std::time::Duration::from_secs(5), connect).await {
            Ok(Ok(conn)) => Ok(conn),
            Ok(Err(e)) => Err(DesktopError::DesktopSessionUnavailable {
                reason: format!("无法连接 AT-SPI accessibility bus: {e}"),
            }),
            Err(_) => Err(DesktopError::DesktopSessionUnavailable {
                reason: "连接 AT-SPI accessibility bus 超时（5 秒）".to_string(),
            }),
        }
    }

    /// 构造根桌面对象代理。
    async fn root_proxy(
        conn: &atspi::zbus::Connection,
    ) -> Result<AccessibleProxy<'_>, DesktopError> {
        // 用 builder 直接构造，避免 ObjectRefExt 返回值借用局部 ObjectRef。
        // destination 用 well-known name（org.a11y.atspi.Registry），由 zbus 解析到当前注册表实例。
        let dest = atspi::zbus::names::WellKnownName::from_static_str(ROOT_DEST).map_err(|e| {
            DesktopError::BackendUnavailable {
                reason: format!("无效根服务名: {e}"),
            }
        })?;
        let path = atspi::zbus::zvariant::ObjectPath::from_static_str(ROOT_PATH).map_err(|e| {
            DesktopError::BackendUnavailable {
                reason: format!("无效根路径: {e}"),
            }
        })?;
        let builder = AccessibleProxy::builder(conn)
            .destination(dest)
            .map_err(|e| DesktopError::BackendUnavailable {
                reason: format!("构造根代理失败: {e}"),
            })?
            .path(path)
            .map_err(|e| DesktopError::BackendUnavailable {
                reason: format!("构造根代理路径失败: {e}"),
            })?;
        builder
            .build()
            .await
            .map_err(|e| DesktopError::BackendUnavailable {
                reason: format!("连接根桌面对象失败: {e}"),
            })
    }
}

#[async_trait]
impl Backend for LinuxBackend {
    fn platform(&self) -> Platform {
        Platform::Linux
    }

    async fn status(&self) -> DesktopResult<StatusInfo> {
        match self.connect().await {
            Ok(_conn) => DesktopResult::Ok(StatusInfo {
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
        let conn = match self.connect().await {
            Ok(c) => c,
            Err(e) => return DesktopResult::Err(e),
        };
        let zbus = conn.connection();
        let desktop = match Self::root_proxy(zbus).await {
            Ok(d) => d,
            Err(e) => return DesktopResult::Err(e),
        };
        let children = match desktop.get_children().await {
            Ok(c) => c,
            Err(e) => {
                return DesktopResult::Err(DesktopError::BackendUnavailable {
                    reason: format!("读取桌面子应用失败: {e}"),
                });
            }
        };
        let snapshot = self.next_snapshot();
        let mut windows = Vec::new();
        for (idx, child_ref) in children.into_iter().enumerate() {
            let proxy = match child_ref.as_accessible_proxy(zbus).await {
                Ok(p) => p,
                Err(_) => continue,
            };
            let name = proxy.name().await.unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            windows.push(WindowInfo {
                app_name: name.clone(),
                pid: 0,
                element: ElementRef {
                    id: format!("atspi-app-{idx}"),
                    snapshot,
                },
                title: name,
                is_foreground: false,
                bounds: Bounds::default(),
                visible: true,
                enabled: true,
                identifiers: StableIdentifiers {
                    automation_id: None,
                    role: Some("AXApplication".to_string()),
                },
            });
        }
        if let Some(app_name) = req.app_name.as_deref() {
            let needle = app_name.to_lowercase();
            windows.retain(|w| w.app_name.to_lowercase().contains(&needle));
        }
        DesktopResult::Ok(tiangong_plugin_computer_use_protocol::ListWindowsResponse { windows })
    }

    async fn snapshot(&self, req: &SnapshotRequest) -> DesktopResult<SnapshotInfo> {
        let conn = match self.connect().await {
            Ok(c) => c,
            Err(e) => return DesktopResult::Err(e),
        };
        let zbus = conn.connection();
        let desktop = match Self::root_proxy(zbus).await {
            Ok(d) => d,
            Err(e) => return DesktopResult::Err(e),
        };
        // 必须指定 app_name 才能定位到具体应用，避免遍历整个桌面。
        let app_name = match req.scope.app_name.as_deref() {
            Some(n) => n,
            None => {
                return DesktopResult::Err(DesktopError::ApplicationNotFound {
                    query: "AT-SPI snapshot 需要指定 app_name".to_string(),
                });
            }
        };
        // 从桌面根的子应用中按名称找到对应应用的代理作为遍历根。
        let children = match desktop.get_children().await {
            Ok(c) => c,
            Err(e) => {
                return DesktopResult::Err(DesktopError::BackendUnavailable {
                    reason: format!("读取桌面子应用失败: {e}"),
                });
            }
        };
        let needle = app_name.to_lowercase();
        let mut root = None;
        for child_ref in children {
            if let Ok(proxy) = child_ref.as_accessible_proxy(zbus).await {
                let name = proxy.name().await.unwrap_or_default();
                if name.to_lowercase().contains(&needle) {
                    root = Some(proxy);
                    break;
                }
            }
        }
        let root = match root {
            Some(p) => p,
            None => {
                return DesktopResult::Err(DesktopError::ApplicationNotFound {
                    query: app_name.to_string(),
                });
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
        self.walk(
            &root,
            zbus,
            snapshot,
            0,
            max_depth,
            max_nodes,
            req.include_invisible,
            None,
            &mut nodes,
            &mut truncated,
        )
        .await;
        DesktopResult::Ok(SnapshotInfo {
            snapshot,
            nodes,
            truncated,
            warnings,
        })
    }

    async fn find(&self, _req: &FindRequest) -> DesktopResult<FindInfo> {
        // AT-SPI 后端的 find 需缓存快照节点表，当前尚未实现。
        // 返回 BackendUnavailable 表示后端能力未实现（而非控件不支持动作）。
        DesktopResult::Err(DesktopError::BackendUnavailable {
            reason: "AT-SPI 后端的 find 尚未实现".to_string(),
        })
    }

    async fn action(&self, req: &ActionRequest) -> DesktopResult<ActionResult> {
        // AT-SPI 后端的 action 需从缓存还原 ObjectRef 并调 Action 接口，当前尚未实现。
        let _ = req;
        DesktopResult::Err(DesktopError::BackendUnavailable {
            reason: "AT-SPI 后端的 action 尚未实现".to_string(),
        })
    }

    async fn wait(&self, req: &WaitRequest) -> DesktopResult<WaitResult> {
        // appear/disappear 基于窗口存在性轮询；控件级等待依赖快照缓存，当前返回超时。
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
            _ => {
                // 控件级等待（focus/available/value）需缓存 ObjectRef 并轮询，当前未实现。
                // 返回 BackendUnavailable 明确表示能力未实现，而非假装等待了完整时间。
                let _ = req;
                DesktopResult::Err(DesktopError::BackendUnavailable {
                    reason: "AT-SPI 后端的控件级等待尚未实现".to_string(),
                })
            }
        }
    }
}

impl LinuxBackend {
    /// 递归读取控件树（前序遍历）。
    #[allow(clippy::too_many_arguments)]
    async fn walk(
        &self,
        element: &AccessibleProxy<'_>,
        conn: &atspi::zbus::Connection,
        snapshot: u64,
        depth: u32,
        max_depth: u32,
        max_nodes: usize,
        include_invisible: bool,
        parent_id: Option<String>,
        nodes: &mut Vec<ControlNode>,
        truncated: &mut bool,
    ) {
        if nodes.len() >= max_nodes || depth > max_depth {
            *truncated = true;
            return;
        }
        // 读取属性；单控件失败不影响整棵树。
        let role = element.get_role().await.unwrap_or(Role::Unknown);
        let name = element.name().await.unwrap_or_default();
        let state = element.get_state().await.unwrap_or_default();
        let visible = state.contains(State::Visible);
        if !include_invisible && !visible {
            return;
        }
        let enabled = state.contains(State::Enabled);
        let focused = state.contains(State::Focused);
        let interfaces = element
            .get_interfaces()
            .await
            .unwrap_or_else(|_| InterfaceSet::empty());
        let actions = supported_actions_for(role, &interfaces);

        let id = format!("atspi-{snapshot}-{}", nodes.len());
        let parent_index = nodes.len();
        nodes.push(ControlNode {
            element: ElementRef {
                id: id.clone(),
                snapshot,
            },
            role: format!("{role:?}"),
            name,
            identifiers: StableIdentifiers {
                automation_id: None,
                role: Some(format!("{role:?}")),
            },
            value: None,
            sensitive: false,
            visible,
            enabled,
            focused,
            bounds: Bounds::default(),
            actions,
            parent: parent_id.map(|pid| ElementRef { id: pid, snapshot }),
            children: Vec::new(),
        });

        // 递归子节点。
        let children = match element.get_children().await {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut child_refs = Vec::with_capacity(children.len());
        for child_ref in children {
            let child = match child_ref.as_accessible_proxy(conn).await {
                Ok(p) => p,
                Err(_) => continue,
            };
            let before = nodes.len();
            Box::pin(self.walk(
                &child,
                conn,
                snapshot,
                depth + 1,
                max_depth,
                max_nodes,
                include_invisible,
                Some(id.clone()),
                nodes,
                truncated,
            ))
            .await;
            if nodes.len() > before {
                child_refs.push(nodes[before].element.clone());
            }
        }
        if let Some(node) = nodes.get_mut(parent_index) {
            node.children = child_refs;
        }
    }
}

/// 按 AT-SPI 角色和已实现接口推断控件支持的动作。
fn supported_actions_for(role: Role, interfaces: &InterfaceSet) -> Vec<ActionKind> {
    use ActionKind::*;
    if !interfaces.contains(Interface::Action) {
        return Vec::new();
    }
    match role {
        Role::PushButton | Role::ToggleButton | Role::CheckBox | Role::Menu => vec![Press],
        Role::Entry | Role::Text => vec![SetValue, Focus],
        Role::Slider | Role::SpinButton => vec![SetValue],
        Role::RadioButton | Role::ListItem => vec![Select],
        Role::ComboBox => vec![Expand, Collapse],
        _ => Vec::new(),
    }
}
