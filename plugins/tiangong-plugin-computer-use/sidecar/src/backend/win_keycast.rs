//! 按键可视化 HUD（Windows）：与 macOS `keycast` 同样式同时序。
//!
//! 主屏工作区中下部显示 [「天工」靛紫胶囊徽标 | 键帽… | ×N] 卡片；`type`
//! 文本输入不显示。连续按键时每一步各占一张卡片：新卡片出现在底部槽位，
//! 已有卡片平滑向上挤，每张卡片按自己的出现时间独立计时淡出（同一组键
//! 连按合并为 ×N 计数），避免单个 HUD 反复重绘造成的快速闪烁。面板与键帽的圆角填充由本模块逐像素
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

/// 每张卡片完全显示时长（自出现起计），之后开始淡出。
const HOLD: Duration = Duration::from_millis(1200);
/// 每帧（≈16ms）淡出步长。
const FADE_STEP: f64 = 0.08;
/// 每帧淡入步长（新卡片约 4 帧完全显现）。
const FADE_IN_STEP: f64 = 0.25;
/// 超出栈容量被挤出的卡片加速淡出步长。
const EVICT_FADE_STEP: f64 = 0.2;
/// 同时显示的卡片上限；超出时最早的卡片加速淡出。
const MAX_CARDS: usize = 5;
/// 卡片之间的竖直间距（96 DPI 像素）。
const STACK_GAP: f64 = 10.0;
/// 上挤动画：每帧向目标位置指数趋近的比例。
const SLIDE_EASE: f64 = 0.3;
/// 新卡片从槽位下方该距离（96 DPI 像素）滑入。
const ENTER_OFFSET: f64 = 16.0;
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

// ── 卡片栈（纯逻辑，便于单测）─────────────────────────────────

/// 卡片生命周期阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 淡入/保持中，到期前不透明。
    Visible,
    /// 正常到期淡出。
    Fading,
    /// 被挤出栈，加速淡出。
    Evicted,
}

/// 栈中一张卡片的状态（不含窗口资源）。
#[derive(Debug, Clone, PartialEq)]
struct Card {
    id: u64,
    keys: Vec<String>,
    repeat: u32,
    height: f64,
    /// 到期时间：出现（或合并连按）时刻 + HOLD。
    hold_until: Instant,
    alpha: f64,
    /// 当前竖直偏移（相对底部槽位顶边，向上为负）。
    offset: f64,
    phase: Phase,
}

/// `push` 的结果：新建卡片或合并到最新卡片（需要重绘 ×N）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pushed {
    New(u64),
    Merged(u64),
}

/// 卡片栈：`cards` 按出现先后排列（末尾最新，位于底部槽位）。
#[derive(Debug, Default)]
struct Stack {
    cards: Vec<Card>,
    next_id: u64,
    gap: f64,
    enter_offset: f64,
}

impl Stack {
    fn new(scale: f64) -> Self {
        Self {
            cards: Vec::new(),
            next_id: 0,
            gap: STACK_GAP * scale,
            enter_offset: ENTER_OFFSET * scale,
        }
    }

    /// 压入一组按键。与最新卡片相同且其尚未开始淡出时合并计数并重置其计时；
    /// 否则新建卡片置于底部，超出容量的最早卡片转为加速淡出。
    fn push(&mut self, keys: &[String], height: f64, now: Instant) -> Pushed {
        if let Some(last) = self.cards.last_mut()
            && last.phase == Phase::Visible
            && last.keys == keys
        {
            last.repeat += 1;
            last.hold_until = now + HOLD;
            last.alpha = 1.0;
            return Pushed::Merged(last.id);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.cards.push(Card {
            id,
            keys: keys.to_vec(),
            repeat: 1,
            height,
            hold_until: now + HOLD,
            alpha: 0.0,
            offset: self.enter_offset,
            phase: Phase::Visible,
        });
        let alive = self
            .cards
            .iter()
            .filter(|c| c.phase != Phase::Evicted)
            .count();
        if alive > MAX_CARDS {
            let overflow = alive - MAX_CARDS;
            for card in self
                .cards
                .iter_mut()
                .filter(|c| c.phase != Phase::Evicted)
                .take(overflow)
            {
                card.phase = Phase::Evicted;
            }
        }
        Pushed::New(id)
    }

    /// 各卡片的目标偏移：最新卡片为 0，越早的卡片越靠上（负值）。
    fn targets(&self) -> Vec<f64> {
        let mut targets = vec![0.0; self.cards.len()];
        let mut y = 0.0;
        for (i, card) in self.cards.iter().enumerate().rev() {
            if i + 1 < self.cards.len() {
                y -= card.height + self.gap;
            }
            targets[i] = y;
        }
        targets
    }

    /// 推进一帧：淡入、到期淡出、上挤滑动；返回本帧被移除的卡片 id。
    fn advance(&mut self, now: Instant) -> Vec<u64> {
        let targets = self.targets();
        for (card, target) in self.cards.iter_mut().zip(targets) {
            match card.phase {
                Phase::Visible if now >= card.hold_until => card.phase = Phase::Fading,
                _ => {}
            }
            card.alpha = match card.phase {
                Phase::Visible => (card.alpha + FADE_IN_STEP).min(1.0),
                Phase::Fading => (card.alpha - FADE_STEP).max(0.0),
                Phase::Evicted => (card.alpha - EVICT_FADE_STEP).max(0.0),
            };
            let next = card.offset + (target - card.offset) * SLIDE_EASE;
            card.offset = if (next - target).abs() < 0.5 {
                target
            } else {
                next
            };
        }
        let mut removed = Vec::new();
        self.cards.retain(|card| {
            let done = card.phase != Phase::Visible && card.alpha <= 0.0;
            if done {
                removed.push(card.id);
            }
            !done
        });
        removed
    }
}

// ── 窗口层 ────────────────────────────────────────────────────

/// 一张卡片的窗口资源。
struct CardWindow {
    id: u64,
    hwnd: HWND,
    surface: Surface,
    width: f64,
    shown: bool,
}

/// 按键 HUD：卡片栈 + 每张卡片一个分层窗口（窗口可复用）。
pub struct KeyCastHud {
    fonts: Fonts,
    scale: f64,
    stack: Stack,
    windows: Vec<CardWindow>,
    /// 空闲窗口池（卡片移除后回收）。
    pool: Vec<HWND>,
    /// 天工截图期间暂时隐藏（内容与淡出计时照常推进）。
    suspended: bool,
}

impl KeyCastHud {
    /// 创建 HUD（在 overlay 线程调用）。预先建一个窗口，确认分层窗口可用。
    pub fn new(scale: f64) -> Option<Self> {
        let first = create_layered_window()?;
        Some(Self {
            fonts: Fonts::new(scale),
            scale,
            stack: Stack::new(scale),
            windows: Vec::new(),
            pool: vec![first],
            suspended: false,
        })
    }

    fn render_card(&self, keys: &[String], repeat: u32) -> Option<(Surface, f64, f64)> {
        let fonts = &self.fonts;
        let layout = layout(keys, repeat, self.scale, &mut |text, kind| {
            measure_text(fonts.get(kind), text)
        });
        let surface = render(&layout, &self.fonts, self.scale)?;
        Some((surface, layout.width, layout.height))
    }

    /// 显示一组键帽：新卡片进入底部槽位，已有卡片向上挤。
    pub fn show(&mut self, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        let height = (PANEL_HEIGHT * self.scale).round();
        match self.stack.push(keys, height, Instant::now()) {
            Pushed::Merged(id) => {
                let Some(card) = self.stack.cards.iter().find(|c| c.id == id) else {
                    return;
                };
                let (keys, repeat) = (card.keys.clone(), card.repeat);
                if let Some((surface, width, _)) = self.render_card(&keys, repeat)
                    && let Some(window) = self.windows.iter_mut().find(|w| w.id == id)
                {
                    window.surface = surface;
                    window.width = width;
                }
            }
            Pushed::New(id) => {
                let Some((surface, width, _)) = self.render_card(keys, 1) else {
                    self.stack.cards.retain(|c| c.id != id);
                    return;
                };
                let Some(hwnd) = self.pool.pop().or_else(create_layered_window) else {
                    self.stack.cards.retain(|c| c.id != id);
                    return;
                };
                self.windows.push(CardWindow {
                    id,
                    hwnd,
                    surface,
                    width,
                    shown: false,
                });
            }
        }
        self.present_all();
    }

    /// 截图期间隐藏 / 截图后恢复。
    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
        for window in &self.windows {
            if !window.shown {
                continue;
            }
            if suspended {
                hide(window.hwnd);
            } else {
                show_topmost(window.hwnd);
            }
        }
    }

    /// 每帧推进：各卡片独立淡入/淡出，上挤滑动，淡出完毕的窗口回收。
    pub fn tick(&mut self) {
        if self.stack.cards.is_empty() {
            return;
        }
        for id in self.stack.advance(Instant::now()) {
            if let Some(index) = self.windows.iter().position(|w| w.id == id) {
                let window = self.windows.remove(index);
                hide(window.hwnd);
                self.pool.push(window.hwnd);
            }
        }
        self.present_all();
    }

    /// 按卡片当前位置与透明度呈现全部窗口。
    fn present_all(&mut self) {
        let work = primary_work_area();
        let slot_height = (PANEL_HEIGHT * self.scale).round();
        for card in &self.stack.cards {
            let Some(window) = self.windows.iter_mut().find(|w| w.id == card.id) else {
                continue;
            };
            let (x, y) = hud_origin(work, window.width, slot_height);
            let y = y + card.offset.round() as i32;
            window.surface.present(window.hwnd, x, y, card.alpha);
            if !window.shown {
                window.shown = true;
                if !self.suspended {
                    show_topmost(window.hwnd);
                }
            }
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

    fn keys(list: &[&str]) -> Vec<String> {
        names(list)
    }

    #[test]
    fn stack_pushes_new_cards_to_bottom_and_older_upwards() {
        let now = Instant::now();
        let mut stack = Stack::new(1.0);
        stack.push(&keys(&["Ctrl", "A"]), 64.0, now);
        stack.push(&keys(&["Ctrl", "C"]), 64.0, now);
        stack.push(&keys(&["End"]), 64.0, now);
        assert_eq!(stack.targets(), vec![-148.0, -74.0, 0.0]);
    }

    #[test]
    fn stack_merges_repeated_keys_into_newest_card() {
        let now = Instant::now();
        let mut stack = Stack::new(1.0);
        assert_eq!(stack.push(&keys(&["Enter"]), 64.0, now), Pushed::New(0));
        assert_eq!(
            stack.push(&keys(&["Enter"]), 64.0, now + Duration::from_millis(300)),
            Pushed::Merged(0)
        );
        assert_eq!(stack.cards.len(), 1);
        assert_eq!(stack.cards[0].repeat, 2);
        assert_eq!(
            stack.cards[0].hold_until,
            now + Duration::from_millis(300) + HOLD
        );
        // 不同键 → 新卡片；再按 Enter 不会合并到旧卡片。
        assert_eq!(stack.push(&keys(&["Tab"]), 64.0, now), Pushed::New(1));
        assert_eq!(stack.push(&keys(&["Enter"]), 64.0, now), Pushed::New(2));
    }

    #[test]
    fn cards_expire_independently_by_their_own_appear_time() {
        let t0 = Instant::now();
        let mut stack = Stack::new(1.0);
        stack.push(&keys(&["A"]), 64.0, t0);
        stack.push(&keys(&["B"]), 64.0, t0 + Duration::from_millis(500));
        // 先推进若干帧让两张卡片淡入完毕。
        for _ in 0..5 {
            stack.advance(t0 + Duration::from_millis(600));
        }
        // 第一张到期，第二张仍在保持期。
        let t1 = t0 + HOLD + Duration::from_millis(10);
        stack.advance(t1);
        assert_eq!(stack.cards[0].phase, Phase::Fading);
        assert_eq!(stack.cards[1].phase, Phase::Visible);
        let mut removed = Vec::new();
        for _ in 0..30 {
            removed.extend(stack.advance(t1));
        }
        assert_eq!(removed, vec![0]);
        assert_eq!(stack.cards.len(), 1);
        assert_eq!(stack.cards[0].id, 1);
        assert_eq!(stack.cards[0].alpha, 1.0);
        // 剩下的卡片目标回到底部槽位（未被移除的卡片不上移）。
        assert_eq!(stack.targets(), vec![0.0]);
    }

    #[test]
    fn overflow_evicts_oldest_cards() {
        let now = Instant::now();
        let mut stack = Stack::new(1.0);
        for i in 0..(MAX_CARDS + 2) {
            stack.push(&[format!("F{i}")], 64.0, now);
        }
        let evicted: Vec<u64> = stack
            .cards
            .iter()
            .filter(|c| c.phase == Phase::Evicted)
            .map(|c| c.id)
            .collect();
        assert_eq!(evicted, vec![0, 1]);
        let mut removed = Vec::new();
        for _ in 0..10 {
            removed.extend(stack.advance(now));
        }
        assert_eq!(removed, vec![0, 1]);
        assert_eq!(stack.cards.len(), MAX_CARDS);
    }

    #[test]
    fn new_card_slides_in_and_fades_in() {
        let now = Instant::now();
        let mut stack = Stack::new(1.0);
        stack.push(&keys(&["A"]), 64.0, now);
        assert_eq!(stack.cards[0].alpha, 0.0);
        assert_eq!(stack.cards[0].offset, ENTER_OFFSET);
        for _ in 0..20 {
            stack.advance(now);
        }
        assert_eq!(stack.cards[0].alpha, 1.0);
        assert_eq!(stack.cards[0].offset, 0.0);
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
