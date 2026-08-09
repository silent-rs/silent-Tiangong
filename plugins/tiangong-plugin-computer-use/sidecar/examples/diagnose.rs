//! Computer Use 诊断程序：直接调用当前平台后端，打印真实的会话/授权状态、
//! 窗口列表与控件树快照。
//!
//! 用法：cargo run -p tiangong-plugin-computer-use-sidecar --example diagnose

use tiangong_plugin_computer_use_protocol::ops::{ListWindowsRequest, SnapshotRequest};
use tiangong_plugin_computer_use_protocol::{DesktopResult, Platform};
use tiangong_plugin_computer_use_sidecar::backend::{self, Backend};

// Backend trait 用于解析 backend 实例的异步方法调用。
#[allow(unused_imports)]
use Backend as _Backend;

#[tokio::main]
async fn main() {
    println!("=== Computer Use 后端诊断 ===");
    println!("编译目标平台: {:?}", Platform::CURRENT);

    let backend = backend::current_backend();
    println!("后端报告平台: {:?}", backend.platform());

    println!("\n--- desktop_status ---");
    match backend.status().await {
        DesktopResult::Ok(info) => {
            println!("会话状态: {:?}", info.session);
            println!(
                "无障碍能力可用: {} (原因: {:?})",
                info.accessibility.available, info.accessibility.reason
            );
            println!("支持的动作: {:?}", info.supported_actions);
        }
        DesktopResult::Err(e) => {
            println!("错误: {}", e.agent_message());
        }
    }

    println!("\n--- desktop_list_windows ---");
    let req = ListWindowsRequest::default();
    match backend.list_windows(&req).await {
        DesktopResult::Ok(resp) => {
            println!("发现窗口数: {}", resp.windows.len());
            for w in resp.windows.iter().take(15) {
                println!(
                    "  - {} (pid={}, 前台={})",
                    w.app_name, w.pid, w.is_foreground
                );
            }
        }
        DesktopResult::Err(e) => {
            println!("错误: {}", e.agent_message());
        }
    }

    // 控件树快照：选一个前台且有界面的应用。
    println!("\n--- desktop_snapshot ---");
    // 优先选访达（常驻、有按钮，便于验证 find/action），其次前台应用。
    let target_name = match backend.list_windows(&ListWindowsRequest::default()).await {
        DesktopResult::Ok(r) => r
            .windows
            .iter()
            .find(|w| w.app_name.contains("访达") || w.app_name.contains("Finder"))
            .or_else(|| {
                r.windows
                    .iter()
                    .find(|w| w.is_foreground && !w.app_name.is_empty())
            })
            .or_else(|| r.windows.iter().find(|w| !w.app_name.is_empty()))
            .map(|w| w.app_name.clone()),
        _ => None,
    };
    match target_name {
        Some(name) => {
            println!("快照目标应用: {name}");
            let snap_req = SnapshotRequest {
                scope: tiangong_plugin_computer_use_protocol::ops::SnapshotScope {
                    window: None,
                    app_name: Some(name.clone()),
                    pid: None,
                },
                max_depth: 0,
                max_nodes: 0,
                include_invisible: false,
                access: Default::default(),
            };
            match backend.snapshot(&snap_req).await {
                DesktopResult::Ok(info) => {
                    println!(
                        "快照版本: {}, 节点数: {}, 截断: {}",
                        info.snapshot,
                        info.nodes.len(),
                        info.truncated
                    );
                    if !info.warnings.is_empty() {
                        println!("告警: {}", info.warnings.join("; "));
                    }
                    for n in info.nodes.iter().take(20) {
                        let value = n
                            .value
                            .as_deref()
                            .map(|v| format!("值={v}"))
                            .unwrap_or_default();
                        let bounds = if n.bounds.width > 0.0 || n.bounds.height > 0.0 {
                            format!(
                                " 边界=({},{},{},{})",
                                n.bounds.x as i32,
                                n.bounds.y as i32,
                                n.bounds.width as i32,
                                n.bounds.height as i32
                            )
                        } else {
                            String::new()
                        };
                        println!(
                            "  - [{}] {} {} {}{}",
                            n.role,
                            n.name,
                            if n.sensitive { "(敏感)" } else { "" },
                            value,
                            bounds
                        );
                    }
                }
                DesktopResult::Err(e) => {
                    println!("快照错误: {}", e.agent_message());
                }
            }
        }
        None => println!("未找到可快照的应用"),
    }

    // find + action：取快照后用 find 在快照内查找按钮，再对其执行 focus。
    println!("\n--- desktop_find + desktop_action(focus) ---");
    use tiangong_plugin_computer_use_protocol::ops::{
        ActionRequest, ActionRequestKind, FindConditions, FindRequest, SnapshotScope,
    };
    // 先取应用快照，再用快照版本调 find。优先访达。
    let target_app = match backend.list_windows(&ListWindowsRequest::default()).await {
        DesktopResult::Ok(r) => r
            .windows
            .iter()
            .find(|w| w.app_name.contains("访达") || w.app_name.contains("Finder"))
            .or_else(|| {
                r.windows
                    .iter()
                    .find(|w| w.is_foreground && !w.app_name.is_empty())
            })
            .map(|w| w.app_name.clone()),
        _ => None,
    };
    let find_result = if let Some(name) = target_app {
        let snap = backend
            .snapshot(&SnapshotRequest {
                scope: SnapshotScope {
                    window: None,
                    app_name: Some(name),
                    pid: None,
                },
                max_depth: 0,
                max_nodes: 0,
                include_invisible: false,
                access: Default::default(),
            })
            .await;
        match snap {
            DesktopResult::Ok(info) => {
                // 用快照版本调真实 find API。
                let find_req = FindRequest {
                    window: None,
                    snapshot: Some(info.snapshot),
                    conditions: FindConditions {
                        role: Some("AXButton".to_string()),
                        ..Default::default()
                    },
                    max_candidates: 1,
                    access: Default::default(),
                };
                match backend.find(&find_req).await {
                    DesktopResult::Ok(find_info) => {
                        println!(
                            "find 命中按钮数: {}, 歧义: {}",
                            find_info.matches.len(),
                            find_info.ambiguous
                        );
                        if let Some(target) = find_info.matches.first() {
                            println!(
                                "目标按钮: [{}] {} (引用 {}, 快照 {})",
                                target.role,
                                target.name,
                                target.element.id,
                                target.element.snapshot
                            );
                            Some(target.element.clone())
                        } else {
                            None
                        }
                    }
                    DesktopResult::Err(e) => {
                        println!("find 错误: {}", e.agent_message());
                        None
                    }
                }
            }
            DesktopResult::Err(e) => {
                println!("快照错误: {}", e.agent_message());
                None
            }
        }
    } else {
        println!("未找到前台应用");
        None
    };

    // 对找到的按钮执行 focus 动作。
    if let Some(element) = find_result {
        let action_req = ActionRequest {
            element,
            action: ActionRequestKind::Focus,
            value: None,
            selection: None,
            access: Default::default(),
        };
        match backend.action(&action_req).await {
            DesktopResult::Ok(r) => {
                println!(
                    "focus 动作结果: performed={}, 说明={}",
                    r.performed, r.summary
                );
            }
            DesktopResult::Err(e) => println!("focus 动作错误: {}", e.agent_message()),
        }
    }
}
