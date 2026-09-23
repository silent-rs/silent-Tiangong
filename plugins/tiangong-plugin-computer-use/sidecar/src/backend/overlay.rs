//! 天工虚拟指针 overlay（RFC 0018，仅 macOS）。
//!
//! `desktop_action` 走 AX 语义动作（AXPress/AXSetValue 等），系统鼠标
//! 指针纹丝不动，用户无法感知 Agent 的操作落点。本模块在 sidecar 进程
//! 内自建一个置顶、点击穿透的小窗口，把「天工指针」（白底箭头 + 靛紫
//! 渐变描边 + 右下角「天工」徽标）平滑移动到目标控件中心。指针默认
//! 关闭，由 Agent 经 `virtual_cursor` 工具持久开关（RFC 0018）：开启后
//! 出现在系统鼠标当前位置并常驻跟随动作落点，不自动淡出；关闭后淡出。
//! 实现完全位于插件内，不依赖宿主与前端；指针图片源文件
//! `resources/virtual-cursor.svg` 与嵌入式浏览器共用（RFC 0018 预留）。
//!
//! 线程模型：AppKit 断言 `NSApplication`/`NSWindow` 只能在进程主线程
//! 使用（其他线程触发 Objective-C 异常，Rust 无法捕获会直接 abort）。
//! 因此 sidecar 的 `main` 把服务循环交给工作线程，主线程专跑
//! [`run_main_loop`]——持有本进程唯一的 `NSApplication`
//! （ActivationPolicy::Prohibited，无 Dock 图标、不抢焦点），手动泵
//! 事件循环驱动窗口绘制与淡出；`show_at` 经全局 channel 非阻塞投递，
//! 可在任意线程调用；服务收尾时经 `request_shutdown` 结束循环。
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained};
use objc2::{MainThreadMarker, class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent,
    NSEventMask, NSImage, NSImageView, NSScreen, NSStatusWindowLevel, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSData, NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize};

/// 指针内容尺寸（points）；嵌入 PNG 为 512x384（4x）。
const CONTENT_W: f64 = 64.0;
const CONTENT_H: f64 = 48.0;
/// 箭头尖端（hotspot）相对内容左上角的偏移（points），与 SVG 规格
/// `(16,12)/2` 对齐。
const HOTSPOT_X: f64 = 8.0;
const HOTSPOT_Y: f64 = 6.0;
/// 泵节奏 ≈60fps，配合逐帧插值产生平滑移动。
const PUMP_INTERVAL: Duration = Duration::from_millis(16);
const FADE_STEP: f64 = 0.05;
/// 平滑移动：每帧向目标位置指数趋近的比例（60fps 下约 200ms 到达）。
const MOVE_EASE: f64 = 0.16;
/// 距目标小于该距离（points）即吸附，结束移动。
const MOVE_EPSILON: f64 = 0.5;

/// 指针 PNG（由 resources/virtual-cursor.svg 按同规格生成，见
/// resources/gen_cursor_png.swift）。
const CURSOR_PNG: &[u8] = include_bytes!("../../../resources/virtual-cursor.png");

static SENDER: OnceLock<Sender<Command>> = OnceLock::new();
static RECEIVER: Mutex<Option<Receiver<Command>>> = Mutex::new(None);
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// overlay 命令。
enum Command {
    /// 平滑移动到目标（AX 坐标）；仅在开启状态下生效。
    MoveTo { x: f64, y: f64 },
    /// 持久开关：开启时常驻（接管系统鼠标位置），关闭时淡出。
    SetEnabled(bool),
}

/// 平滑移动指针到屏幕坐标（points，AX 全局坐标系：主屏左上原点）。
///
/// 目标点应为被操作控件的中心；箭头尖端对准该点。指针关闭时命令被
/// 忽略。非阻塞投递、失败静默——指针是纯增益可视化，不允许拖垮
/// 桌面动作本身；overlay 主循环未运行（如单元测试进程）时丢弃。
pub fn move_to(x: f64, y: f64) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.send(Command::MoveTo { x, y });
    }
}

/// 持久开关指针（Agent 经 `virtual_cursor` 工具驱动）。
///
/// 开启：指针出现在系统鼠标当前位置并常驻，后续 `move_to` 平滑跟随；
/// 关闭：淡出并忽略 `move_to`。非阻塞投递。
pub fn set_enabled(enabled: bool) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.send(Command::SetEnabled(enabled));
    }
}

/// 请求 overlay 主循环退出（服务线程收尾时调用）。
pub fn request_shutdown() {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

/// overlay 主循环：必须在进程主线程调用（占用直至进程退出）。
///
/// 初始化全局 channel 与 `NSApplication`，随后以 30ms 节奏泵事件、
/// 处理指针命令并推进淡出；`request_shutdown` 后返回。
pub fn run_main_loop() {
    let (tx, rx) = mpsc::channel();
    let _ = SENDER.set(tx);
    *RECEIVER.lock().unwrap() = Some(rx);
    let mtm = MainThreadMarker::new().expect("overlay 主循环必须在进程主线程运行");
    let app = NSApplication::sharedApplication(mtm);
    // 无 Dock 图标、不参与激活、永不抢用户焦点。
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    let Some(window) = build_window(mtm) else {
        tracing::warn!("虚拟指针 overlay 窗口创建失败，指针可视化不可用");
        wait_for_shutdown();
        return;
    };
    let receiver = RECEIVER.lock().unwrap().take();
    let mut visible_until: Option<Instant> = None;
    let mut alpha: f64 = 0.0;
    // 持久开关状态：默认关闭，Agent 经 virtual_cursor 工具切换。
    let mut enabled = false;
    // 平滑移动状态：current 为当前 AX 坐标，target 存在时逐帧趋近。
    // 首次出现或淡出后（不可见）直接落位，可见时连续滑动。
    let mut current: Option<(f64, f64)> = None;
    let mut target: Option<(f64, f64)> = None;
    let mut screen_top = main_screen_top(mtm);
    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            return;
        }
        let mut dirty = false;
        if let Some(rx) = receiver.as_ref() {
            while let Ok(command) = rx.try_recv() {
                match command {
                    // 关闭态忽略移动；开启态更新目标（常驻不淡出）。
                    Command::MoveTo { x, y } => {
                        if enabled {
                            target = Some((x, y));
                            alpha = 1.0;
                            visible_until = None;
                            screen_top = main_screen_top(mtm);
                            dirty = true;
                        }
                    }
                    Command::SetEnabled(true) => {
                        enabled = true;
                        // 接管系统鼠标当前位置（AppKit 左下原点 → AX
                        // 左上原点），指针在此常驻显示。
                        let location = NSEvent::mouseLocation();
                        current = Some((location.x, screen_top - location.y));
                        target = None;
                        alpha = 1.0;
                        visible_until = None;
                        screen_top = main_screen_top(mtm);
                        dirty = true;
                    }
                    Command::SetEnabled(false) => {
                        enabled = false;
                        target = None;
                        // 立即进入淡出。
                        visible_until = Some(Instant::now());
                        dirty = true;
                    }
                }
            }
        }
        // 平滑移动：每帧向目标指数趋近（自带 ease-out），像真实鼠标
        // 连续滑动；到达后吸附并结束移动。
        if let Some((tx, ty)) = target {
            let landed = match current {
                None => Some((tx, ty)),
                Some(_) if alpha <= 0.0 => Some((tx, ty)),
                Some((cx, cy)) => {
                    let nx = cx + (tx - cx) * MOVE_EASE;
                    let ny = cy + (ty - cy) * MOVE_EASE;
                    if (nx - tx).abs() < MOVE_EPSILON && (ny - ty).abs() < MOVE_EPSILON {
                        Some((tx, ty))
                    } else {
                        current = Some((nx, ny));
                        None
                    }
                }
            };
            if let Some(position) = landed {
                current = Some(position);
                target = None;
            }
            if let Some((cx, cy)) = current {
                let origin = window_origin(cx, cy, screen_top);
                window.setFrameOrigin(origin);
            }
            dirty = true;
        }
        pump_events(&app);
        if let Some(deadline) = visible_until
            && Instant::now() >= deadline
        {
            alpha = (alpha - FADE_STEP).max(0.0);
            if alpha == 0.0 {
                visible_until = None;
            }
            dirty = true;
        }
        if dirty {
            window.setAlphaValue(alpha);
        }
        std::thread::sleep(PUMP_INTERVAL);
    }
}

/// 窗口创建失败的兜底等待：保持主线程存活直至服务请求退出，
/// 避免 sidecar 进程过早终止。
fn wait_for_shutdown() {
    while !SHUTDOWN.load(Ordering::Relaxed) {
        std::thread::sleep(PUMP_INTERVAL);
    }
}

/// AX 全局坐标 → overlay 窗口原点（AppKit 全局坐标，主屏左下原点）。
///
/// AX `y` 以主屏左上为原点向下增长；AppKit 以主屏左下为原点向上增长，
/// `screen_top` 为主屏顶边的 AppKit y（单主屏时即主屏高度）。纯函数，
/// 便于单测坐标换算。
fn window_origin(x: f64, y: f64, screen_top: f64) -> NSPoint {
    NSPoint::new(x - HOTSPOT_X, screen_top - y + HOTSPOT_Y - CONTENT_H)
}

/// 主屏顶边的 AppKit y 坐标；读取失败时返回 0（指针纵向偏移，
/// 不影响动作本身）。
fn main_screen_top(mtm: MainThreadMarker) -> f64 {
    NSScreen::mainScreen(mtm)
        .map(|screen| {
            let frame = screen.frame();
            frame.origin.y + frame.size.height
        })
        .unwrap_or(0.0)
}

/// 非阻塞泵出并派发全部挂起的 AppKit 事件（驱动窗口首次绘制等）。
fn pump_events(app: &NSApplication) {
    let past = NSDate::distantPast();
    while let Some(event) = unsafe {
        app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask(u64::MAX),
            Some(&past),
            NSDefaultRunLoopMode,
            true,
        )
    } {
        app.sendEvent(&event);
    }
}

fn build_window(_mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
    let frame = NSRect::new(
        NSPoint::new(-2.0 * CONTENT_W, -2.0 * CONTENT_H),
        NSSize::new(CONTENT_W, CONTENT_H),
    );
    unsafe {
        // SAFETY：NSWindow 类存在且 alloc 返回未初始化实例，随后立即 init。
        let allocated: Allocated<NSWindow> = msg_send![class!(NSWindow), alloc];
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            allocated,
            frame,
            NSWindowStyleMask::empty(),
            NSBackingStoreType::Buffered,
            false,
        );
        // NSStatusWindowLevel：盖过普通应用窗口与工具栏。
        window.setLevel(NSStatusWindowLevel);
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setIgnoresMouseEvents(true);
        window.setHasShadow(false);
        window.setAlphaValue(0.0);
        // 纯 ARC 环境：窗口关闭不得触发 release 悬挂。
        window.setReleasedWhenClosed(false);
        let data = NSData::with_bytes(CURSOR_PNG);
        // SAFETY：同上，alloc 后立即 initWithData。
        let allocated_image: Allocated<NSImage> = msg_send![class!(NSImage), alloc];
        let image = NSImage::initWithData(allocated_image, &data)?;
        image.setSize(NSSize::new(CONTENT_W, CONTENT_H));
        let allocated_view: Allocated<NSImageView> = msg_send![class!(NSImageView), alloc];
        let view = NSImageView::initWithFrame(allocated_view, frame);
        view.setImage(Some(&image));
        window.setContentView(Some(&view));
        // 不抢 key/main 状态：用户当前操作零干扰。
        window.orderFrontRegardless();
        Some(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_origin_places_hotspot_on_target() {
        // 主屏高 900：AX (640, 500) → 尖端应落在该点。
        let origin = window_origin(640.0, 500.0, 900.0);
        // 尖端 = 窗口原点 + hotspot 偏移；窗口顶 = origin.y + CONTENT_H，
        // 尖端 AppKit y = origin.y + CONTENT_H - HOTSPOT_Y。
        assert_eq!(origin.x, 640.0 - HOTSPOT_X);
        let tip_y = origin.y + CONTENT_H - HOTSPOT_Y;
        assert_eq!(tip_y, 900.0 - 500.0);
    }

    #[test]
    fn window_origin_at_screen_top_left() {
        // AX (0,0)（主屏左上角）→ 窗口不越过屏幕顶边。
        let origin = window_origin(0.0, 0.0, 900.0);
        assert_eq!(origin.x, -HOTSPOT_X);
        assert_eq!(origin.y + CONTENT_H, 900.0 + HOTSPOT_Y);
    }
}
