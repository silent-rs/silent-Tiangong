//! 按键 HUD 快速连按演示（仅 Windows）：只驱动 HUD 显示，不向系统发送任何按键。
//!
//! 用法：cargo run -p tiangong-plugin-computer-use-sidecar --example keycast_demo
//!
//! 依次演示：
//! 1. 快速连按不同快捷键（每 120ms 一次）：卡片逐个向上挤，按各自出现时间淡出；
//! 2. 同一个键连按：合并为一张卡片并显示 ×N；
//! 3. 超过同屏上限的连按：最早的卡片加速淡出；
//! 4. 间隔较慢的按键（每 700ms 一次）：可以看到上面的卡片先于下面的卡片消失。

#[cfg(target_os = "windows")]
fn main() {
    use std::thread::sleep;
    use std::time::Duration;
    use tiangong_plugin_computer_use_sidecar::backend::win_overlay::key_cast;

    // 与 sidecar 相同：声明 Per-Monitor-V2 DPI 感知，HUD 尺寸与位置按物理像素计算。
    // SAFETY：进程级一次性设置，失败可忽略。
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }

    let keys = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let stage = |title: &str| {
        println!("\n== {title}");
        sleep(Duration::from_millis(300));
    };

    stage("1. 快速连按不同快捷键（间隔 120ms）");
    for combo in [
        &["Ctrl", "A"][..],
        &["Ctrl", "C"],
        &["End"],
        &["Ctrl", "V"],
        &["Enter"],
    ] {
        println!("   {}", combo.join("+"));
        key_cast(keys(combo));
        sleep(Duration::from_millis(120));
    }
    sleep(Duration::from_millis(2200));

    stage("2. 同一个键连按 6 次（合并为 ×N）");
    for _ in 0..6 {
        key_cast(keys(&["↓"]));
        sleep(Duration::from_millis(150));
    }
    sleep(Duration::from_millis(2000));

    stage("3. 连按 9 个不同键（超过同屏 5 张上限）");
    for i in 1..=9 {
        let name = format!("F{i}");
        println!("   {name}");
        key_cast(vec![name]);
        sleep(Duration::from_millis(100));
    }
    sleep(Duration::from_millis(2200));

    stage("4. 慢速按键（间隔 700ms，观察各自独立退出）");
    for combo in [&["Ctrl", "S"][..], &["Alt", "Tab"], &["Win", "↓"], &["Esc"]] {
        println!("   {}", combo.join("+"));
        key_cast(keys(combo));
        sleep(Duration::from_millis(700));
    }
    sleep(Duration::from_millis(2500));
    println!("\n演示结束");
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("keycast_demo 仅支持 Windows");
}
