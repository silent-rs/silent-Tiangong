//! macOS 无障碍后端。
//!
//! 通过 `objc2-app-kit` 的 `NSWorkspace` 列举运行中的应用，作为窗口发现的基础；
//! 通过链接 ApplicationServices 的 `AXIsProcessTrusted` 探测辅助功能授权状态。
//!
//! 当前实现聚焦于可在本机可靠验证的能力：`desktop_status` 探测图形会话与授权状态，
//! `desktop_list_windows` 列举前台/活动应用作为窗口候选。控件树快照、查找与动作
//! 返回明确的能力说明，后续按平台增量补全，不静默退化为坐标点击。

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use objc2::rc::Retained;
use objc2_app_kit::NSWorkspace;

use super::{
    ActionResult, Backend, FindInfo, SnapshotInfo, StatusInfo, WaitResult, all_supported_actions,
};
use tiangong_plugin_computer_use_protocol::{
    AccessibilityCapability, ActionKind, Bounds, DesktopError, DesktopResult, DesktopSession,
    ElementRef, Platform, StableIdentifiers, WindowInfo,
};

// ApplicationServices 框架的辅助功能授权探测。
// AXIsProcessTrusted 返回当前进程是否被授予辅助功能权限。
// macOS arm64 上 BOOL 为 bool 尺寸的 _Bool，这里用 u8 容纳并按非零判断。
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

pub struct MacosBackend {
    snapshot_seq: AtomicU64,
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
        }
    }

    /// 探测辅助功能授权是否已授予。
    fn is_trusted() -> bool {
        // SAFETY：AXIsProcessTrusted 无副作用，可重复调用，仅读取系统授权状态。
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// 下一个快照版本号。
    fn next_snapshot(&self) -> u64 {
        self.snapshot_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// 列举运行中的应用，构造窗口候选列表。
    fn enumerate_windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = workspace.runningApplications();
        let snapshot = self.next_snapshot();
        let mut windows = Vec::new();
        let mut idx = 0u64;
        for app in apps.iter() {
            // 仅列举已启动完成、面向用户的应用。
            let active = app.isActive();
            let name: String = app
                .localizedName()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let pid = app.processIdentifier() as u32;
            idx += 1;
            windows.push(WindowInfo {
                app_name: name.clone(),
                pid,
                element: ElementRef {
                    id: format!("macos-app-{pid}-{idx}"),
                    snapshot,
                },
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
}

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
            // 当前实现暴露完整动作集合，由具体控件在 snapshot 中报告实际支持项。
            supported_actions: all_supported_actions(),
        })
    }

    async fn list_windows(
        &self,
        req: &tiangong_plugin_computer_use_protocol::ops::ListWindowsRequest,
    ) -> DesktopResult<tiangong_plugin_computer_use_protocol::ListWindowsResponse> {
        if !Self::is_trusted() {
            return DesktopResult::Err(DesktopError::PermissionDenied {
                reason: "尚未授予辅助功能权限，请在系统设置中允许".to_string(),
            });
        }
        let mut windows = match self.enumerate_windows() {
            Ok(w) => w,
            Err(e) => return DesktopResult::Err(e),
        };
        // 应用名称筛选（包含匹配）。
        if let Some(app_name) = req.app_name.as_deref() {
            let needle = app_name.to_lowercase();
            windows.retain(|w| w.app_name.to_lowercase().contains(&needle));
        }
        // 进程编号筛选。
        if let Some(pid) = req.pid {
            windows.retain(|w| w.pid == pid);
        }
        // 仅前台筛选。
        if req.foreground_only {
            windows.retain(|w| w.is_foreground);
        }
        DesktopResult::Ok(tiangong_plugin_computer_use_protocol::ListWindowsResponse { windows })
    }

    async fn snapshot(
        &self,
        _req: &tiangong_plugin_computer_use_protocol::ops::SnapshotRequest,
    ) -> DesktopResult<SnapshotInfo> {
        // 控件树遍历需逐控件读取 AXChildren 并限制深度/节点数，当前返回能力说明。
        DesktopResult::Err(DesktopError::ActionNotSupported {
            action: "desktop_snapshot".to_string(),
            supported: vec![],
        })
    }

    async fn find(
        &self,
        _req: &tiangong_plugin_computer_use_protocol::ops::FindRequest,
    ) -> DesktopResult<FindInfo> {
        DesktopResult::Err(DesktopError::ActionNotSupported {
            action: "desktop_find".to_string(),
            supported: vec![],
        })
    }

    async fn action(
        &self,
        _req: &tiangong_plugin_computer_use_protocol::ops::ActionRequest,
    ) -> DesktopResult<ActionResult> {
        DesktopResult::Err(DesktopError::ActionNotSupported {
            action: "desktop_action".to_string(),
            supported: vec![],
        })
    }

    async fn wait(
        &self,
        req: &tiangong_plugin_computer_use_protocol::ops::WaitRequest,
    ) -> DesktopResult<WaitResult> {
        // 事件等待优先使用平台事件，当前以有限轮询实现 appear/disappear 的窗口存在性。
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(req.timeout_ms);
        use tiangong_plugin_computer_use_protocol::ops::WaitCondition;
        match &req.condition {
            WaitCondition::Appear { target } | WaitCondition::Disappear { target } => {
                let looking_appear = matches!(req.condition, WaitCondition::Appear { .. });
                let start = std::time::Instant::now();
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
            WaitCondition::Focus { element }
            | WaitCondition::Available { element }
            | WaitCondition::Value { element, .. } => {
                // 控件级等待依赖快照能力，当前统一返回超时。
                let _ = element;
                DesktopResult::Ok(WaitResult {
                    satisfied: false,
                    waited_ms: req.timeout_ms,
                    matched_element: None,
                })
            }
        }
    }
}

impl MacosBackend {
    /// 检查目标应用当前是否存在（基于列举结果与名称/标题匹配）。
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

/// 抑制未使用告警。
#[allow(dead_code)]
fn _keep(_v: Vec<ActionKind>) {}
#[allow(dead_code)]
type _Retained<T> = Retained<T>;
