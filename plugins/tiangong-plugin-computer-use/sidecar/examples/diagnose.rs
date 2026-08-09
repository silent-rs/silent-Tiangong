//! Computer Use 诊断程序：直接调用当前平台后端，打印真实的会话/授权状态与窗口列表。
//!
//! 用法：cargo run -p tiangong-plugin-computer-use-sidecar --example diagnose

use tiangong_plugin_computer_use_protocol::ops::ListWindowsRequest;
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
            println!("结构: {e:?}");
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
            println!("结构: {e:?}");
        }
    }
}
