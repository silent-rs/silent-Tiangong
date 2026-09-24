//! 天工虚拟指针 overlay（Windows）：与 macOS `overlay` 同接口同语义。
//!
//! 指针是一个置顶、点击穿透、不激活的分层窗口（`WS_EX_LAYERED |
//! WS_EX_TRANSPARENT`），逐像素 alpha 由
//! `UpdateLayeredWindow` 呈现。指针图片与 macOS 共用 `virtual-cursor.png`，
//! 经 WIC 按主屏 DPI 缩放并转为预乘 BGRA；点击脉冲预生成若干放大帧。
//!
//! 线程模型：Win32 窗口必须由创建它的线程泵消息，因此首次调用任一接口时
//! 惰性启动专用 overlay 线程，接口经 channel 非阻塞投递命令；`glide_and_wait`
//! 通过到达回执同步等待动画完成（先移动到位、后触发点击）。按键 HUD 由
//! 同一线程驱动（见 `win_keycast`）。
//!
//! 截图排除：不使用 `WDA_EXCLUDEFROMCAPTURE`——它同样会把指针/HUD 从
//! RustDesk、向日葵、OBS 等基于屏幕捕获的远程桌面与录屏画面中抹掉，演示
//! 时对方看不到。改为天工自己截图时经 [`hide_for_capture`] 同步短暂隐藏，
//! 截完立即恢复。
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP,
    HDC, HGDIOBJ, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint, ReleaseDC, SelectObject,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, IWICPalette,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnLoad,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, HWND_TOPMOST, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SetWindowPos, ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

/// 指针内容尺寸（96 DPI 下的像素）；嵌入 PNG 为 512x384。
const CONTENT_W: f64 = 64.0;
const CONTENT_H: f64 = 48.0;
/// 箭头尖端（hotspot）相对内容左上角的偏移（96 DPI 像素）。
const HOTSPOT_X: f64 = 8.0;
const HOTSPOT_Y: f64 = 6.0;
/// 泵节奏 ≈60fps。
const PUMP_INTERVAL: Duration = Duration::from_millis(16);
const FADE_STEP: f64 = 0.05;
/// 平滑移动：每帧向目标指数趋近的比例。
const MOVE_EASE: f64 = 0.16;
/// 距目标小于该距离（像素）即吸附。
const MOVE_EPSILON: f64 = 0.5;
/// 点击脉冲：总时长、最大放大系数与预生成帧数。
const PULSE_MS: u64 = 280;
const PULSE_SCALE: f64 = 0.25;
const PULSE_FRAMES: usize = 6;
/// 分身空闲自动隐藏时长（轮次结束钩子未送达时兜底）。
const IDLE_HIDE: Duration = Duration::from_secs(120);
/// 每隔该时长重申一次置顶（其他置顶窗口出现后仍保持在最上层）。
const TOPMOST_REFRESH: Duration = Duration::from_millis(500);
/// 截图隐藏的回执等待上限（overlay 线程异常时不阻塞截图）。
const CAPTURE_ACK_TIMEOUT: Duration = Duration::from_millis(250);
/// 截图隐藏的兜底恢复时长（恢复命令丢失时指针/HUD 不会永久消失）。
const CAPTURE_MAX_SUSPEND: Duration = Duration::from_secs(3);

const CURSOR_PNG: &[u8] = include_bytes!("../../../resources/virtual-cursor.png");
const WINDOW_CLASS: PCWSTR = w!("TiangongComputerUseOverlay");

static SENDER: OnceLock<Sender<Command>> = OnceLock::new();
static OVERLAY_READY: AtomicBool = AtomicBool::new(false);
static ARRIVAL: Mutex<(u64, (f64, f64))> = Mutex::new((0, (0.0, 0.0)));
static ARRIVAL_CVAR: Condvar = Condvar::new();

enum Command {
    MoveTo {
        x: f64,
        y: f64,
        summon: bool,
    },
    SetEnabled(bool),
    ClickPulse {
        x: f64,
        y: f64,
    },
    KeyCast(Vec<String>),
    /// 截图前隐藏指针与 HUD；处理完毕（含 DWM 合成刷新）后经回执通知。
    SuspendForCapture(Sender<()>),
    /// 截图完成，恢复显示。
    ResumeAfterCapture,
}

/// 惰性启动 overlay 线程并返回命令通道；线程启动失败时返回 None（可视化
/// 静默降级，不影响动作本身）。
fn sender() -> Option<&'static Sender<Command>> {
    if let Some(sender) = SENDER.get() {
        return Some(sender);
    }
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("tiangong-overlay".to_string())
        .spawn(move || run_loop(rx));
    if let Err(error) = spawned {
        tracing::warn!(%error, "overlay 线程启动失败，指针可视化不可用");
        return None;
    }
    // 并发首调时仅第一个通道生效，多余线程因通道断开立即退出。
    let _ = SENDER.set(tx);
    SENDER.get()
}

fn post(command: Command) {
    if let Some(sender) = sender() {
        let _ = sender.send(command);
    }
}

/// 截图期间的隐藏守卫：析构时恢复指针与 HUD。
pub struct CaptureGuard {
    active: bool,
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        if self.active
            && let Some(sender) = SENDER.get()
        {
            let _ = sender.send(Command::ResumeAfterCapture);
        }
    }
}

/// 截图前同步隐藏天工指针与按键 HUD，返回的守卫析构时恢复。overlay 线程
/// 未启动（本轮未显示过）时直接返回空守卫，不为截图惰性创建窗口。
pub fn hide_for_capture() -> CaptureGuard {
    let Some(sender) = SENDER.get() else {
        return CaptureGuard { active: false };
    };
    let (ack_tx, ack_rx) = mpsc::channel();
    if sender.send(Command::SuspendForCapture(ack_tx)).is_err() {
        return CaptureGuard { active: false };
    }
    let _ = ack_rx.recv_timeout(CAPTURE_ACK_TIMEOUT);
    CaptureGuard { active: true }
}

/// 显示按键 HUD（键帽符号序列）。
pub fn key_cast(keys: Vec<String>) {
    post(Command::KeyCast(keys));
}

/// 平滑移动指针到屏幕坐标；本轮首次调用时从系统鼠标位置分身出现。
pub fn move_to(x: f64, y: f64) {
    post(Command::MoveTo { x, y, summon: true });
}

/// 仅在指针已显示时跟随到目标（UIA 语义动作用）。
pub fn follow_to(x: f64, y: f64) {
    post(Command::MoveTo {
        x,
        y,
        summon: false,
    });
}

/// 点击脉冲。
pub fn click_pulse(x: f64, y: f64) {
    post(Command::ClickPulse { x, y });
}

/// 开关指针（轮次结束时关闭收起分身）。
pub fn set_enabled(enabled: bool) {
    post(Command::SetEnabled(enabled));
}

/// 平滑移动指针到目标并等待动画真正到达（带超时兜底）。
pub fn glide_and_wait(x: f64, y: f64) {
    const ARRIVAL_TIMEOUT: Duration = Duration::from_millis(1500);
    let Some(sender) = sender() else {
        return;
    };
    // 首次调用时等待窗口就绪（最多 300ms），之后未就绪说明创建失败。
    let ready_deadline = Instant::now() + Duration::from_millis(300);
    while !OVERLAY_READY.load(Ordering::Acquire) {
        if Instant::now() >= ready_deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let before = ARRIVAL.lock().unwrap().0;
    if sender.send(Command::MoveTo { x, y, summon: true }).is_err() {
        return;
    }
    let deadline = Instant::now() + ARRIVAL_TIMEOUT;
    let mut state = ARRIVAL.lock().unwrap();
    while state.0 <= before || !arrived_at(state.1, (x, y)) {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let (guard, _) = ARRIVAL_CVAR.wait_timeout(state, deadline - now).unwrap();
        state = guard;
    }
}

fn arrived_at(point: (f64, f64), target: (f64, f64)) -> bool {
    (point.0 - target.0).abs() < MOVE_EPSILON && (point.1 - target.1).abs() < MOVE_EPSILON
}

/// 指针窗口左上角（屏幕像素）：hotspot 对准目标点。
fn window_origin(x: f64, y: f64, dpi_scale: f64, pulse: f64) -> (i32, i32) {
    (
        (x - HOTSPOT_X * dpi_scale * pulse).round() as i32,
        (y - HOTSPOT_Y * dpi_scale * pulse).round() as i32,
    )
}

// ── 分层窗口绘制面 ────────────────────────────────────────────

/// 32bpp 自顶向下 DIB 绘制面（预乘 BGRA），供 `UpdateLayeredWindow` 呈现。
pub(super) struct Surface {
    pub dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    pub width: i32,
    pub height: i32,
}

impl Surface {
    pub fn new(width: i32, height: i32) -> Option<Self> {
        let width = width.max(1);
        let height = height.max(1);
        // SAFETY：创建内存 DC 与 DIB，失败时逐一释放；bits 由系统分配，
        // 生命周期与 bitmap 一致。
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(bitmap) if !bits.is_null() => bitmap,
                _ => {
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
            let old = SelectObject(dc, bitmap.into());
            Some(Self {
                dc,
                bitmap,
                old,
                bits: bits.cast(),
                width,
                height,
            })
        }
    }

    pub fn pixels(&mut self) -> &mut [u8] {
        // SAFETY：bits 指向 width*height*4 字节的 DIB 像素区，独占借用。
        unsafe {
            std::slice::from_raw_parts_mut(self.bits, (self.width * self.height * 4) as usize)
        }
    }

    /// 以常量透明度把绘制面呈现到分层窗口的屏幕位置。
    pub fn present(&self, hwnd: HWND, x: i32, y: i32, alpha: f64) {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let position = POINT { x, y };
        let size = SIZE {
            cx: self.width,
            cy: self.height,
        };
        let source = POINT::default();
        // SAFETY：hwnd 为本线程创建的分层窗口；DC 在调用后立即释放。
        unsafe {
            let screen = GetDC(None);
            let _ = UpdateLayeredWindow(
                hwnd,
                Some(screen),
                Some(&position),
                Some(&size),
                Some(self.dc),
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            ReleaseDC(None, screen);
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY：按创建逆序归还 GDI 对象。
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY：默认处理。
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// 创建置顶、点击穿透、不激活、不进入截图的分层窗口（隐藏状态）。
pub(super) fn create_layered_window() -> Option<HWND> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    // SAFETY：注册窗口类与创建窗口均在 overlay 线程内完成。
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let registered = *REGISTERED.get_or_init(|| {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: WINDOW_CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class) != 0
        });
        if !registered {
            return None;
        }
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            WINDOW_CLASS,
            w!("Tiangong"),
            WS_POPUP,
            -2000,
            -2000,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        Some(hwnd)
    }
}

/// 显示（不激活）并重申置顶。
pub(super) fn show_topmost(hwnd: HWND) {
    // SAFETY：本线程窗口。
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

pub(super) fn hide(hwnd: HWND) {
    // SAFETY：本线程窗口。
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// 主屏 DPI 缩放系数（96 DPI = 1.0）。
pub(super) fn primary_dpi_scale() -> f64 {
    // SAFETY：纯查询。
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() && dpi_x > 0
        {
            f64::from(dpi_x) / 96.0
        } else {
            1.0
        }
    }
}

fn system_cursor() -> Option<(f64, f64)> {
    let mut point = POINT::default();
    // SAFETY：point 有效可写。
    unsafe { GetCursorPos(&mut point) }
        .ok()
        .map(|()| (f64::from(point.x), f64::from(point.y)))
}

/// 用 WIC 把嵌入 PNG 解码并缩放为指定尺寸的预乘 BGRA 绘制面。
fn decode_cursor(width: u32, height: u32) -> windows::core::Result<Surface> {
    // SAFETY：WIC COM 调用，所有接口在本函数内创建与释放；CopyPixels
    // 写入大小正好为 stride*height 的 DIB 缓冲。
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(CURSOR_PNG)?;
        let decoder = factory.CreateDecoderFromStream(
            &stream,
            std::ptr::null(),
            WICDecodeMetadataCacheOnLoad,
        )?;
        let frame = decoder.GetFrame(0)?;
        let scaler = factory.CreateBitmapScaler()?;
        scaler.Initialize(&frame, width, height, WICBitmapInterpolationModeFant)?;
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut surface = Surface::new(width as i32, height as i32)
            .ok_or_else(windows::core::Error::from_win32)?;
        converter.CopyPixels(std::ptr::null(), width * 4, surface.pixels())?;
        Ok(surface)
    }
}

// ── overlay 线程主循环 ────────────────────────────────────────

fn pump_messages() {
    let mut message = MSG::default();
    // SAFETY：本线程消息队列。
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn run_loop(receiver: Receiver<Command>) {
    // SAFETY：本线程 COM 初始化（WIC 需要），重复初始化的返回值可忽略。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let dpi_scale = primary_dpi_scale();
    let frames: Vec<Surface> = (0..=PULSE_FRAMES)
        .filter_map(|i| {
            let pulse = 1.0 + PULSE_SCALE * i as f64 / PULSE_FRAMES as f64;
            decode_cursor(
                (CONTENT_W * dpi_scale * pulse).round() as u32,
                (CONTENT_H * dpi_scale * pulse).round() as u32,
            )
            .map_err(|error| tracing::warn!(%error, "虚拟指针图片解码失败"))
            .ok()
        })
        .collect();
    let window = if frames.len() == PULSE_FRAMES + 1 {
        create_layered_window()
    } else {
        None
    };
    if window.is_none() {
        tracing::warn!("虚拟指针 overlay 窗口创建失败，指针可视化不可用");
    } else {
        OVERLAY_READY.store(true, Ordering::Release);
    }
    let mut keycast = super::win_keycast::KeyCastHud::new(dpi_scale);
    if keycast.is_none() {
        tracing::warn!("按键 HUD 窗口创建失败，按键可视化不可用");
    }

    let mut enabled = false;
    let mut shown = false;
    let mut alpha: f64 = 0.0;
    let mut fading = false;
    let mut last_activity = Instant::now();
    let mut last_topmost = Instant::now();
    let mut current: Option<(f64, f64)> = None;
    let mut target: Option<(f64, f64)> = None;
    let mut pulse_until: Option<Instant> = None;
    // 截图隐藏计数（并发截图可嵌套）与起始时间（兜底恢复用）。
    let mut capture_depth: u32 = 0;
    let mut capture_since = Instant::now();

    loop {
        let mut dirty = false;
        loop {
            match receiver.try_recv() {
                Ok(Command::MoveTo { x, y, summon }) => {
                    if !enabled && summon {
                        current = system_cursor().or(Some((x, y)));
                        enabled = true;
                    }
                    if enabled {
                        target = Some((x, y));
                        alpha = 1.0;
                        fading = false;
                        last_activity = Instant::now();
                        dirty = true;
                    }
                }
                Ok(Command::SetEnabled(true)) => {
                    if !enabled {
                        current = system_cursor();
                    }
                    enabled = true;
                    target = None;
                    alpha = 1.0;
                    fading = false;
                    last_activity = Instant::now();
                    dirty = true;
                }
                Ok(Command::SetEnabled(false)) => {
                    if enabled {
                        enabled = false;
                        target = None;
                        fading = true;
                    }
                }
                Ok(Command::ClickPulse { x, y }) => {
                    if enabled {
                        target = Some((x, y));
                        pulse_until = Some(Instant::now() + Duration::from_millis(PULSE_MS));
                        last_activity = Instant::now();
                        dirty = true;
                    }
                }
                Ok(Command::KeyCast(keys)) => {
                    if let Some(hud) = keycast.as_mut() {
                        hud.show(&keys);
                    }
                }
                Ok(Command::SuspendForCapture(ack)) => {
                    if capture_depth == 0 {
                        if let (Some(hwnd), true) = (window, shown) {
                            hide(hwnd);
                        }
                        if let Some(hud) = keycast.as_mut() {
                            hud.set_suspended(true);
                        }
                        // 等 DWM 合成出不含指针/HUD 的一帧，再让截图继续。
                        // SAFETY：无参数的同步调用。
                        let _ = unsafe { DwmFlush() };
                    }
                    capture_depth += 1;
                    capture_since = Instant::now();
                    let _ = ack.send(());
                }
                Ok(Command::ResumeAfterCapture) => {
                    if capture_depth > 0 {
                        capture_depth -= 1;
                        if capture_depth == 0 {
                            resume_after_capture(window, shown, keycast.as_mut());
                            last_topmost = Instant::now();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                // 服务退出（发送端全部释放）：结束线程。
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        if capture_depth > 0 && capture_since.elapsed() >= CAPTURE_MAX_SUSPEND {
            capture_depth = 0;
            resume_after_capture(window, shown, keycast.as_mut());
            last_topmost = Instant::now();
        }
        if enabled && target.is_none() && last_activity.elapsed() >= IDLE_HIDE {
            enabled = false;
            fading = true;
        }
        // 平滑移动：每帧指数趋近，到达后吸附并发出到达回执。
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
                if let Ok(mut state) = ARRIVAL.lock() {
                    state.0 += 1;
                    state.1 = position;
                }
                ARRIVAL_CVAR.notify_all();
            }
            dirty = true;
        }
        let pulse = match pulse_until {
            Some(deadline) => {
                let remaining = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as f64;
                let progress = (1.0 - remaining / PULSE_MS as f64).clamp(0.0, 1.0);
                dirty = true;
                if progress >= 1.0 {
                    pulse_until = None;
                    1.0
                } else {
                    1.0 + PULSE_SCALE * (std::f64::consts::PI * progress).sin()
                }
            }
            None => 1.0,
        };
        if fading {
            alpha = (alpha - FADE_STEP).max(0.0);
            if alpha == 0.0 {
                fading = false;
            }
            dirty = true;
        }
        if let (Some(hwnd), true) = (window, dirty) {
            match current {
                Some((cx, cy)) if alpha > 0.0 => {
                    let index =
                        (((pulse - 1.0) / PULSE_SCALE) * PULSE_FRAMES as f64).round() as usize;
                    let frame = &frames[index.min(PULSE_FRAMES)];
                    let (x, y) = window_origin(cx, cy, dpi_scale, pulse);
                    frame.present(hwnd, x, y, alpha);
                    if !shown {
                        shown = true;
                        if capture_depth == 0 {
                            show_topmost(hwnd);
                            last_topmost = Instant::now();
                        }
                    }
                }
                _ => {
                    if shown {
                        hide(hwnd);
                        shown = false;
                    }
                }
            }
        }
        if let Some(hwnd) = window
            && shown
            && capture_depth == 0
            && last_topmost.elapsed() >= TOPMOST_REFRESH
        {
            show_topmost(hwnd);
            last_topmost = Instant::now();
        }
        if let Some(hud) = keycast.as_mut() {
            hud.tick();
        }
        pump_messages();
        std::thread::sleep(PUMP_INTERVAL);
    }
}

/// 截图结束：恢复逻辑上应显示的指针与 HUD。
fn resume_after_capture(
    window: Option<HWND>,
    shown: bool,
    keycast: Option<&mut super::win_keycast::KeyCastHud>,
) {
    if let (Some(hwnd), true) = (window, shown) {
        show_topmost(hwnd);
    }
    if let Some(hud) = keycast {
        hud.set_suspended(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_origin_places_hotspot_on_target() {
        assert_eq!(window_origin(640.0, 500.0, 1.0, 1.0), (632, 494));
        // 150% 缩放：hotspot 偏移同比放大。
        assert_eq!(window_origin(640.0, 500.0, 1.5, 1.0), (628, 491));
        // 副屏在主屏左侧（负坐标）。
        assert_eq!(window_origin(-100.0, 20.0, 1.0, 1.0), (-108, 14));
    }

    #[test]
    fn arrival_uses_epsilon() {
        assert!(arrived_at((10.2, 10.0), (10.0, 10.3)));
        assert!(!arrived_at((11.0, 10.0), (10.0, 10.0)));
    }
}
