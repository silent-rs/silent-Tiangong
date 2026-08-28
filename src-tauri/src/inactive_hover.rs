//! macOS 后台窗口悬停监听。
//!
//! macOS 只向激活应用派发 mouseMoved，窗口在后台时 CSS `:hover` 不生效，
//! 消息列表右侧导航（刻度尺与按钮组）因此无法唤出。本模块在窗口未激活
//! 时轮询全局鼠标位置，鼠标位于窗口内则把窗口内逻辑坐标（CSS 像素、
//! 左上原点）经 `window:inactive_cursor` 事件下发给前端，由前端对照导航
//! 热区矩形自行决定是否显示导航；payload 为 null 表示离开窗口或窗口
//! 已激活（激活后由 CSS `:hover` 接管）。
//! 窗口从未激活转入激活且鼠标刚在窗口内时，经 `window:inactive_click`
//! 补发首击位置——系统首击被窗口激活消费、未到达页面（wry 的
//! acceptsFirstMouse 挂在 WKWebView 上，实际接收点击的内部 WKContentView
//! 不受其控制），前端据位置执行横条/预览卡跳转。

//!
//! 坐标一律在全局逻辑（点）坐标系比较：NSEvent::mouseLocation 与
//! CGDisplayBounds 同系（左下原点），窗口物理位置按其所在屏缩放换算回
//! 逻辑后同系。不能用 tauri `cursor_position()`——它按主屏缩放统一换算，
//! 混合 DPI 多屏（窗口在非主屏）时与窗口自身坐标系错位，命中判定恒假。

use core_graphics::display::CGDisplay;
use objc2_app_kit::NSEvent;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// 采样间隔。导航本身有 200ms 过渡动画，50ms 采样延迟无感，
/// 且仅在「窗口未激活 + 鼠标在窗口内」时才产生事件流量。
const POLL_INTERVAL_MS: u64 = 50;

/// 位置变化超过该阈值（逻辑像素）才下发，避免静止时重复事件。
const MOVE_THRESHOLD: f64 = 1.0;

/// 窗口从未激活转入激活时，鼠标在窗口内的最近确认距今小于该值视为后台首击。
/// 取两倍采样间隔，覆盖静止悬停后（无坐标下发）点击的情况。
const CLICK_WINDOW_MS: u64 = 150;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InactiveCursorPoint {
    pub x: f64,
    pub y: f64,
}

/// 全局鼠标位置（逻辑点，左上原点）。
fn global_mouse_top_left() -> Option<(f64, f64)> {
    // mouseLocation 为左下原点全局逻辑坐标；用所有活动显示器逻辑 bounds
    // 的最大 y 翻转为左上原点（CGDisplayBounds 与 mouseLocation 同系）。
    let loc = NSEvent::mouseLocation();
    let displays = CGDisplay::active_displays().ok()?;
    let max_y = displays
        .iter()
        .map(|&id| {
            let bounds = CGDisplay::new(id).bounds();
            bounds.origin.y + bounds.size.height
        })
        .fold(0.0_f64, f64::max);
    Some((loc.x, max_y - loc.y))
}

/// 启动后台悬停轮询线程。窗口关闭（拿不到主窗口）即退出。
pub fn spawn(app: AppHandle) {
    std::thread::Builder::new()
        .name("inactive-hover".into())
        .spawn(move || poll_loop(app))
        .expect("启动 inactive-hover 线程失败");
}

fn poll_loop(app: AppHandle) {
    // 上次下发的窗口内坐标；None 表示当前不在「未激活且鼠标在窗口内」状态
    let mut last: Option<InactiveCursorPoint> = None;
    // 最近一次确认鼠标在窗口内的时刻（含静止未下发坐标的轮次）
    let mut in_window_at: Option<std::time::Instant> = None;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));

        let Some(window) = app.get_webview_window("main") else {
            break;
        };
        // 窗口隐藏（托盘）或最小化时不产生悬停信号
        if !window.is_visible().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
            last = emit_leave(&app, last);
            in_window_at = None;
            continue;
        }
        // 激活时 CSS :hover 生效，前端也在 focus 时清状态；此处只补发一次离开
        if window.is_focused().unwrap_or(true) {
            // 后台首击补发：窗口从未激活转入激活，且极短时间内仍确认过鼠标在窗口内，
            // 视为后台点击（系统首击被窗口激活消费、未到达页面）。把点击位置下发给
            // 前端执行命中判定；Cmd+Tab 等无点击激活因鼠标静止无近期确认而天然排除。
            if let (Some(point), Some(at)) = (last, in_window_at) {
                if at.elapsed() < std::time::Duration::from_millis(CLICK_WINDOW_MS) {
                    tracing::debug!(
                        point_x = point.x,
                        point_y = point.y,
                        "inactive-hover：补发后台首击位置"
                    );
                    let _ = app.emit("window:inactive_click", &point);
                }
            }
            last = emit_leave(&app, last);
            in_window_at = None;
            continue;
        }
        let (Some((mouse_x, mouse_y)), Ok(pos), Ok(size), Ok(scale)) = (
            global_mouse_top_left(),
            window.outer_position(),
            window.outer_size(),
            window.scale_factor(),
        ) else {
            tracing::debug!("inactive-hover：鼠标/窗口几何查询失败，跳过本轮");
            continue;
        };
        if scale <= 0.0 {
            continue;
        }
        // 窗口物理几何换算回全局逻辑坐标后与鼠标同系比较
        let (wx, wy) = (pos.x as f64 / scale, pos.y as f64 / scale);
        let (ww, wh) = (size.width as f64 / scale, size.height as f64 / scale);
        let inside = mouse_x >= wx && mouse_x < wx + ww && mouse_y >= wy && mouse_y < wy + wh;
        if !inside {
            last = emit_leave(&app, last);
            in_window_at = None;
            continue;
        }
        in_window_at = Some(std::time::Instant::now());
        let point = InactiveCursorPoint {
            x: mouse_x - wx,
            y: mouse_y - wy,
        };
        let moved = last
            .map(|p| {
                (p.x - point.x).abs() > MOVE_THRESHOLD || (p.y - point.y).abs() > MOVE_THRESHOLD
            })
            .unwrap_or(true);
        if moved {
            tracing::debug!(
                point_x = point.x,
                point_y = point.y,
                "inactive-hover：下发窗口内鼠标坐标"
            );
            let _ = app.emit("window:inactive_cursor", &point);
            last = Some(point);
        }
    }
}

fn emit_leave(app: &AppHandle, last: Option<InactiveCursorPoint>) -> Option<InactiveCursorPoint> {
    if last.is_some() {
        tracing::debug!("inactive-hover：鼠标离开窗口或窗口已激活，下发 null");
        let _ = app.emit("window:inactive_cursor", serde_json::Value::Null);
    }
    None
}
