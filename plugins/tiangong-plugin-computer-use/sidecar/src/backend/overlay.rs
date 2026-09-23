//! 天工虚拟指针 overlay（RFC 0018，仅 macOS）。
//!
//! `desktop_action` 走 AX 语义动作（AXPress/AXSetValue 等），系统鼠标
//! 指针纹丝不动，用户无法感知 Agent 的操作落点。本模块在 sidecar 进程
//! 内自建一个置顶、点击穿透的小窗口，把「天工指针」（白底箭头 + 靛紫
//! 渐变描边 + 右下角「天工」徽标）显示到目标控件中心，动作完成后自动
//! 淡出。实现完全位于插件内，不依赖宿主与前端；指针图片源文件
//! `resources/virtual-cursor.svg` 与嵌入式浏览器共用（RFC 0018 预留）。
//!
//! 线程模型：首个 `show_at` 懒启动一条专用 AppKit 线程——它持有本进程
//! 唯一的 `NSApplication`（ActivationPolicy::Prohibited，无 Dock 图标、
//! 不抢焦点），手动泵事件循环驱动窗口绘制与淡出；命令经 channel 投递，
//! 不阻塞 AX 调用线程。
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained};
use objc2::{MainThreadMarker, class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEventMask,
    NSImage, NSImageView, NSScreen, NSStatusWindowLevel, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSData, NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize};

/// 指针内容尺寸（points）；嵌入 PNG 为 512x384（4x）。
const CONTENT_W: f64 = 64.0;
const CONTENT_H: f64 = 48.0;
/// 箭头尖端（hotspot）相对内容左上角的偏移（points），与 SVG 规格
/// `(16,12)/2` 对齐。
const HOTSPOT_X: f64 = 8.0;
const HOTSPOT_Y: f64 = 6.0;
/// 置顶层级：NSStatusWindowLevel，盖过普通应用窗口与工具栏。
/// 指针显示保持时间与淡出节奏。
const VISIBLE: Duration = Duration::from_millis(2400);
const PUMP_INTERVAL: Duration = Duration::from_millis(30);
const FADE_STEP: f64 = 0.08;

/// 指针 PNG（由 resources/virtual-cursor.svg 按同规格生成，见
/// resources/gen_cursor_png.swift）。
const CURSOR_PNG: &[u8] = include_bytes!("../../../resources/virtual-cursor.png");

static SENDER: OnceLock<Sender<(f64, f64)>> = OnceLock::new();

/// 在屏幕坐标（points，AX 全局坐标系：主屏左上原点）显示指针。
///
/// 目标点应为被操作控件的中心；箭头尖端对准该点。首次调用懒启动
/// overlay 线程，之后仅投递命令（非阻塞、失败静默——指针是纯增益
/// 可视化，不允许拖垮桌面动作本身）。
pub fn show_at(x: f64, y: f64) {
    let sender = SENDER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("virtual-cursor-overlay".to_string())
            .spawn(move || event_loop(rx));
        tx
    });
    let _ = sender.send((x, y));
}

/// AX 全局坐标 → overlay 窗口原点（AppKit 全局坐标，主屏左下原点）。
///
/// AX `y` 以主屏左上为原点向下增长；AppKit 以主屏左下为原点向上增长，
/// `screen_top` 为主屏顶边的 AppKit y（单主屏时即主屏高度）。纯函数，
/// 便于单测坐标换算。
fn window_origin(x: f64, y: f64, screen_top: f64) -> NSPoint {
    NSPoint::new(x - HOTSPOT_X, screen_top - y + HOTSPOT_Y - CONTENT_H)
}

fn event_loop(rx: Receiver<(f64, f64)>) {
    // SAFETY：本进程唯一的 NSApplication 绑定到本线程；sidecar 主线程
    // 不使用 AppKit（NSWorkspace/AXUIElement 不触发 NSApp 初始化）。
    // CLI 工具在非主线程驱动 AppKit 是成熟实践（无 Dock、不激活）。
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    // 无 Dock 图标、不参与激活、永不抢用户焦点。
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    let Some(window) = build_window(mtm) else {
        tracing::warn!("虚拟指针 overlay 窗口创建失败，指针可视化不可用");
        return;
    };
    let mut visible_until: Option<Instant> = None;
    let mut alpha: f64 = 0.0;
    loop {
        let mut dirty = false;
        while let Ok((x, y)) = rx.try_recv() {
            let origin = window_origin(x, y, main_screen_top(mtm));
            window.setFrameOrigin(origin);
            alpha = 1.0;
            visible_until = Some(Instant::now() + VISIBLE);
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
