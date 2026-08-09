//! Computer Use 诊断程序：直接调用当前平台后端，打印真实的会话/授权状态、
//! 窗口列表与控件树快照。
//!
//! 用法：cargo run -p tiangong-plugin-computer-use-sidecar --example diagnose

use tiangong_plugin_computer_use_protocol::ops::{
    ListWindowsRequest, SnapshotRequest, SnapshotScope,
};
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
    let target_name = match backend.list_windows(&ListWindowsRequest::default()).await {
        DesktopResult::Ok(r) => r
            .windows
            .iter()
            .find(|w| w.is_foreground && !w.app_name.is_empty())
            .or_else(|| r.windows.iter().find(|w| !w.app_name.is_empty()))
            .map(|w| w.app_name.clone()),
        _ => None,
    };
    match target_name {
        Some(name) => {
            println!("快照目标应用: {name}");
            let snap_req = SnapshotRequest {
                scope: SnapshotScope {
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
                        println!(
                            "  - [{}] {} {} {}",
                            n.role,
                            n.name,
                            if n.sensitive { "(敏感)" } else { "" },
                            value
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
}
