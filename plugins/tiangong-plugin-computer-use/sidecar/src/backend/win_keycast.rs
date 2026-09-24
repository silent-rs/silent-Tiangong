//! 按键可视化 HUD（Windows）：与 macOS `keycast` 同样式同时序。
//!
//! 主屏工作区中下部短暂显示 [「天工」靛紫胶囊徽标 | 键帽… | ×N]，约 1.2 秒
//! 后淡出；`type` 文本输入不显示。面板与键帽的圆角填充由本模块逐像素
//! 抗锯齿绘制（预乘 BGRA），文字用 GDI 灰度抗锯齿渲染到蒙版后按覆盖率
//! 合成，最终经 `UpdateLayeredWindow` 以逐像素 alpha 呈现在置顶、点击穿透、
//! 不激活的分层窗口上（天工截图时由 overlay 线程短暂隐藏，远程桌面与录屏
//! 可见）。由 overlay 线程创建与驱动。
use std::time::{Duration, Instant};

use windows::Win32::Foundation::COLORREF;
use windows::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateFontW, DEFAULT_CHARSET,
    DEFAULT_PITCH, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject,
    DrawTextW, FF_DONTCARE, FW_BOLD, FW_SEMIBOLD, GetMonitorInfoW, GetTextExtentPoint32W, HFONT,
    MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, OUT_DEFAULT_PRECIS, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::core::w;

use super::win_overlay::{Surface, create_layered_window, hide, show_topmost};

/// 完全显示时长，之后开始淡出。
const HOLD: Duration = Duration::from_millis(1200);
/// 每帧（≈16ms）淡出步长。
const FADE_STEP: f64 = 0.08;
/// HUD 底边距工作区底边的比例（中下部）。
const BOTTOM_RATIO: f64 = 0.18;

// ── 尺寸（96 DPI 像素，按主屏 DPI 缩放）──
const PANEL_HEIGHT: f64 = 64.0;
const PANEL_PAD_X: f64 = 14.0;
const PANEL_RADIUS: f64 = 18.0;
const BADGE_HEIGHT: f64 = 30.0;
const BADGE_PAD_X: f64 = 12.0;
const BADGE_FONT: f64 = 15.0;
const BADGE_GAP: f64 = 14.0;
const KEY_HEIGHT: f64 = 44.0;
const KEY_MIN_WIDTH: f64 = 44.0;
const KEY_PAD_X: f64 = 12.0;
const KEY_FONT: f64 = 22.0;
const KEY_RADIUS: f64 = 9.0;
const KEY_GAP: f64 = 6.0;
const COUNT_FONT: f64 = 18.0;
const COUNT_GAP: f64 = 10.0;

/// 预乘前的 RGBA 颜色（0..=1）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rgba(f64, f64, f64, f64);

const BRAND: Rgba = Rgba(0.42, 0.36, 0.95, 1.0);
const PANEL_FILL: Rgba = Rgba(0.07, 0.07, 0.07, 0.86);
const PANEL_BORDER: Rgba = Rgba(1.0, 1.0, 1.0, 0.12);
const KEY_FILL: Rgba = Rgba(1.0, 1.0, 1.0, 0.14);
const KEY_BORDER: Rgba = Rgba(1.0, 1.0, 1.0, 0.22);
const WHITE: Rgba = Rgba(1.0, 1.0, 1.0, 1.0);
const COUNT_COLOR: Rgba = Rgba(1.0, 1.0, 1.0, 0.75);

/// 轴对齐矩形（像素，浮点）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// 布局中的一个元素：圆角块（可选）+ 居中文字。
#[derive(Debug, Clone, PartialEq)]
struct Item {
    rect: Rect,
    text: String,
    font: FontKind,
    text_color: Rgba,
    fill: Option<(Rgba, f64, Option<Rgba>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontKind {
    Badge,
    Key,
    Count,
}

/// 布局结果：面板尺寸与元素列表。
#[derive(Debug, Clone, PartialEq)]
struct Layout {
    width: f64,
    height: f64,
    items: Vec<Item>,
}

/// 由各文字实测宽度计算布局（纯函数，自左向右绝对排布，总宽即窗口宽）。
fn layout(
    keys: &[String],
    repeat: u32,
    scale: f64,
    measure: &mut dyn FnMut(&str, FontKind) -> f64,
) -> Layout {
    let s = |v: f64| v * scale;
    let height = s(PANEL_HEIGHT).round();
    let mut items = Vec::new();
    let mut x = s(PANEL_PAD_X);

    let badge_w = measure("天工", FontKind::Badge) + 2.0 * s(BADGE_PAD_X);
    items.push(Item {
        rect: Rect {
            x,
            y: ((height - s(BADGE_HEIGHT)) / 2.0).round(),
            w: badge_w,
            h: s(BADGE_HEIGHT),
        },
        text: "天工".to_string(),
        font: FontKind::Badge,
        text_color: WHITE,
        fill: Some((BRAND, s(BADGE_HEIGHT) / 2.0, None)),
    });
    x += badge_w + s(BADGE_GAP);

    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            x += s(KEY_GAP);
        }
        let w = (measure(key, FontKind::Key) + 2.0 * s(KEY_PAD_X)).max(s(KEY_MIN_WIDTH));
        items.push(Item {
            rect: Rect {
                x,
                y: ((height - s(KEY_HEIGHT)) / 2.0).round(),
                w,
                h: s(KEY_HEIGHT),
            },
            text: key.clone(),
            font: FontKind::Key,
            text_color: WHITE,
            fill: Some((KEY_FILL, s(KEY_RADIUS), Some(KEY_BORDER))),
        });
        x += w;
    }

    if repeat > 1 {
        let text = format!("×{repeat}");
        x += s(COUNT_GAP);
        let w = measure(&text, FontKind::Count);
        items.push(Item {
            rect: Rect {
                x,
                y: 0.0,
                w,
                h: height,
            },
            text,
            font: FontKind::Count,
            text_color: COUNT_COLOR,
            fill: None,
        });
        x += w;
    }
    Layout {
        width: (x + s(PANEL_PAD_X)).ceil(),
        height,
        items,
    }
}

/// 窗口左上角：在工作区内水平居中，底边位于工作区高度 BOTTOM_RATIO 处。
fn hud_origin(work: (f64, f64, f64, f64), width: f64, height: f64) -> (i32, i32) {
    let (left, top, right, bottom) = work;
    let x = left + (right - left - width) / 2.0;
    let y = bottom - (bottom - top) * BOTTOM_RATIO - height;
    (x.round() as i32, y.round() as i32)
}

/// 点到圆角矩形的有向距离（内部为负）。
fn round_rect_distance(px: f64, py: f64, rect: Rect, radius: f64) -> f64 {
    let radius = radius.min(rect.w / 2.0).min(rect.h / 2.0);
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;
    let qx = (px - cx).abs() - (rect.w / 2.0 - radius);
    let qy = (py - cy).abs() - (rect.h / 2.0 - radius);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    outside + qx.max(qy).min(0.0) - radius
}

/// 在预乘 BGRA 缓冲上以 src-over 合成一个像素。
fn blend(pixel: &mut [u8], color: Rgba, coverage: f64) {
    let a = (color.3 * coverage).clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let inv = 1.0 - a;
    let mix = |dst: u8, src: f64| ((src * a * 255.0) + f64::from(dst) * inv).round() as u8;
    pixel[0] = mix(pixel[0], color.2);
    pixel[1] = mix(pixel[1], color.1);
    pixel[2] = mix(pixel[2], color.0);
    pixel[3] = ((a * 255.0) + f64::from(pixel[3]) * inv).round() as u8;
}

/// 描边样式：颜色与宽度（像素）。
type Border = Option<(Rgba, f64)>;

/// 抗锯齿填充圆角矩形，可选描边。
fn fill_round_rect(
    buf: &mut [u8],
    size: (i32, i32),
    rect: Rect,
    radius: f64,
    fill: Rgba,
    border: Border,
) {
    let (width, height) = size;
    let x0 = rect.x.floor().max(0.0) as i32;
    let y0 = rect.y.floor().max(0.0) as i32;
    let x1 = ((rect.x + rect.w).ceil() as i32).min(width);
    let y1 = ((rect.y + rect.h).ceil() as i32).min(height);
    for py in y0..y1 {
        for px in x0..x1 {
            let d = round_rect_distance(f64::from(px) + 0.5, f64::from(py) + 0.5, rect, radius);
            let coverage = (0.5 - d).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let offset = ((py * width + px) * 4) as usize;
            let pixel = &mut buf[offset..offset + 4];
            blend(pixel, fill, coverage);
            if let Some((color, border_width)) = border {
                // 描边：距边界 border_width 以内的环带。
                let ring = (0.5 - (d + border_width).abs() + border_width / 2.0)
                    .clamp(0.0, 1.0)
                    .min(coverage);
                blend(pixel, color, ring);
            }
        }
    }
}

struct Fonts {
    badge: HFONT,
    key: HFONT,
    count: HFONT,
}

impl Fonts {
    fn new(scale: f64) -> Self {
        let make = |size: f64, weight: u32| {
            // SAFETY：创建 GDI 字体，Drop 时释放。
            unsafe {
                CreateFontW(
                    -((size * scale).round() as i32),
                    0,
                    0,
                    0,
                    weight as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    ANTIALIASED_QUALITY,
                    (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                    w!("Microsoft YaHei UI"),
                )
            }
        };
        Self {
            badge: make(BADGE_FONT, FW_BOLD.0),
            key: make(KEY_FONT, FW_SEMIBOLD.0),
            count: make(COUNT_FONT, FW_SEMIBOLD.0),
        }
    }

    fn get(&self, kind: FontKind) -> HFONT {
        match kind {
            FontKind::Badge => self.badge,
            FontKind::Key => self.key,
            FontKind::Count => self.count,
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        // SAFETY：释放本结构创建的字体。
        unsafe {
            for font in [self.badge, self.key, self.count] {
                let _ = DeleteObject(font.into());
            }
        }
    }
}

fn measure_text(font: HFONT, text: &str) -> f64 {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    // SAFETY：临时内存 DC，用后释放。
    unsafe {
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, font.into());
        let _ = GetTextExtentPoint32W(dc, &wide, &mut size);
        SelectObject(dc, old);
        let _ = DeleteDC(dc);
    }
    f64::from(size.cx)
}

/// 主屏工作区（left, top, right, bottom）。
fn primary_work_area() -> (f64, f64, f64, f64) {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY：纯查询。
    let ok = unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        GetMonitorInfoW(monitor, &mut info).as_bool()
    };
    let r = info.rcWork;
    if ok {
        (
            f64::from(r.left),
            f64::from(r.top),
            f64::from(r.right),
            f64::from(r.bottom),
        )
    } else {
        (0.0, 0.0, 1920.0, 1080.0)
    }
}

/// 渲染布局为预乘 BGRA 绘制面。
fn render(layout: &Layout, fonts: &Fonts, scale: f64) -> Option<Surface> {
    let width = layout.width as i32;
    let height = layout.height as i32;
    let mut surface = Surface::new(width, height)?;
    let mut mask = Surface::new(width, height)?;
    {
        let line = scale.max(1.0);
        let buf = surface.pixels();
        buf.fill(0);
        fill_round_rect(
            buf,
            (width, height),
            Rect {
                x: 0.0,
                y: 0.0,
                w: layout.width,
                h: layout.height,
            },
            PANEL_RADIUS * scale,
            PANEL_FILL,
            Some((PANEL_BORDER, line)),
        );
        for item in &layout.items {
            if let Some((fill, radius, border)) = item.fill {
                fill_round_rect(
                    buf,
                    (width, height),
                    item.rect,
                    radius,
                    fill,
                    border.map(|c| (c, line)),
                );
            }
        }
    }
    // 文字：先在全黑蒙版上用白色灰度抗锯齿绘制，再按覆盖率合成颜色。
    for item in &layout.items {
        mask.pixels().fill(0);
        let mut wide: Vec<u16> = item.text.encode_utf16().collect();
        let mut rect = RECT {
            left: item.rect.x.floor() as i32,
            top: item.rect.y.floor() as i32,
            right: (item.rect.x + item.rect.w).ceil() as i32,
            bottom: (item.rect.y + item.rect.h).ceil() as i32,
        };
        // SAFETY：mask.dc 为有效内存 DC，字体在 fonts 生命周期内有效。
        unsafe {
            let old = SelectObject(mask.dc, fonts.get(item.font).into());
            SetBkMode(mask.dc, TRANSPARENT);
            SetTextColor(mask.dc, COLORREF(0x00FF_FFFF));
            DrawTextW(
                mask.dc,
                &mut wide,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(mask.dc, old);
        }
        let coverage: Vec<u8> = mask.pixels().iter().skip(1).step_by(4).copied().collect();
        let buf = surface.pixels();
        for (i, value) in coverage.into_iter().enumerate() {
            if value > 0 {
                blend(
                    &mut buf[i * 4..i * 4 + 4],
                    item.text_color,
                    f64::from(value) / 255.0,
                );
            }
        }
    }
    Some(surface)
}

/// 按键 HUD 窗口。
pub struct KeyCastHud {
    hwnd: HWND,
    fonts: Fonts,
    scale: f64,
    surface: Option<Surface>,
    origin: (i32, i32),
    last: Vec<String>,
    repeat: u32,
    hide_at: Option<Instant>,
    alpha: f64,
    /// 天工截图期间暂时隐藏（内容与淡出计时照常推进）。
    suspended: bool,
}

impl KeyCastHud {
    /// 创建隐藏的 HUD 窗口（在 overlay 线程调用）。
    pub fn new(scale: f64) -> Option<Self> {
        let hwnd = create_layered_window()?;
        Some(Self {
            hwnd,
            fonts: Fonts::new(scale),
            scale,
            surface: None,
            origin: (0, 0),
            last: Vec::new(),
            repeat: 0,
            hide_at: None,
            alpha: 0.0,
            suspended: false,
        })
    }

    /// 显示一组键帽：重建内容并居中于主屏工作区中下部，重置淡出计时。
    pub fn show(&mut self, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        if self.hide_at.is_some() && keys == self.last.as_slice() {
            self.repeat += 1;
        } else {
            self.repeat = 1;
            self.last = keys.to_vec();
        }
        let fonts = &self.fonts;
        let layout = layout(keys, self.repeat, self.scale, &mut |text, kind| {
            measure_text(fonts.get(kind), text)
        });
        let Some(surface) = render(&layout, &self.fonts, self.scale) else {
            return;
        };
        self.origin = hud_origin(primary_work_area(), layout.width, layout.height);
        surface.present(self.hwnd, self.origin.0, self.origin.1, 1.0);
        self.surface = Some(surface);
        if !self.suspended {
            show_topmost(self.hwnd);
        }
        self.alpha = 1.0;
        self.hide_at = Some(Instant::now() + HOLD);
    }

    /// 截图期间隐藏 / 截图后恢复（仅在 HUD 正处于显示周期时恢复）。
    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
        if self.hide_at.is_none() {
            return;
        }
        if suspended {
            hide(self.hwnd);
        } else {
            show_topmost(self.hwnd);
        }
    }

    /// 每帧推进：超过显示时长后逐帧淡出，完全透明后隐藏。
    pub fn tick(&mut self) {
        let Some(deadline) = self.hide_at else {
            return;
        };
        if Instant::now() < deadline {
            return;
        }
        self.alpha = (self.alpha - FADE_STEP).max(0.0);
        if let Some(surface) = &self.surface {
            surface.present(self.hwnd, self.origin.0, self.origin.1, self.alpha);
        }
        if self.alpha == 0.0 {
            self.hide_at = None;
            hide(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// 每字符 10 像素的假测量。
    fn fake_measure(text: &str, _kind: FontKind) -> f64 {
        text.chars().count() as f64 * 10.0
    }

    #[test]
    fn layout_places_badge_keys_and_count_left_to_right() {
        let result = layout(&names(&["Ctrl", "C"]), 3, 1.0, &mut fake_measure);
        assert_eq!(result.items.len(), 4);
        // 徽标 20 + 24 = 44；键帽 max(40+24, 44)=64 与 max(10+24, 44)=44。
        assert_eq!(result.items[0].rect.w, 44.0);
        assert_eq!(result.items[1].rect.x, 14.0 + 44.0 + 14.0);
        assert_eq!(result.items[1].rect.w, 64.0);
        assert_eq!(result.items[2].rect.x, 72.0 + 64.0 + 6.0);
        assert_eq!(result.items[2].rect.w, 44.0);
        assert_eq!(result.items[3].text, "×3");
        // 总宽 = 最后元素右边 + 右内边距。
        let last = &result.items[3].rect;
        assert_eq!(result.width, (last.x + last.w + 14.0).ceil());
        assert_eq!(result.height, 64.0);
    }

    #[test]
    fn layout_scales_with_dpi() {
        let normal = layout(&names(&["A"]), 1, 1.0, &mut fake_measure);
        let scaled = layout(&names(&["A"]), 1, 2.0, &mut |t, _| {
            t.chars().count() as f64 * 20.0
        });
        assert_eq!(scaled.height, normal.height * 2.0);
        assert_eq!(scaled.width, normal.width * 2.0);
    }

    #[test]
    fn hud_is_centered_in_lower_part_of_work_area() {
        let (x, y) = hud_origin((0.0, 0.0, 2560.0, 1400.0), 200.0, 64.0);
        assert_eq!(x + 100, 1280);
        assert_eq!(y, (1400.0 - 1400.0 * BOTTOM_RATIO - 64.0).round() as i32);
    }

    #[test]
    fn round_rect_distance_sign() {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 40.0,
        };
        assert!(round_rect_distance(50.0, 20.0, rect, 10.0) < 0.0);
        assert!(round_rect_distance(-1.0, 20.0, rect, 10.0) > 0.0);
        // 圆角外侧的角点在矩形内但在圆角外。
        assert!(round_rect_distance(0.5, 0.5, rect, 10.0) > 0.0);
    }

    #[test]
    fn blend_is_premultiplied_src_over() {
        let mut pixel = [0u8, 0, 0, 0];
        blend(&mut pixel, Rgba(1.0, 0.0, 0.0, 0.5), 1.0);
        // BGRA：红色预乘 0.5 → R=128，A=128。
        assert_eq!(pixel, [0, 0, 128, 128]);
        blend(&mut pixel, WHITE, 0.0);
        assert_eq!(pixel, [0, 0, 128, 128]);
    }
}
