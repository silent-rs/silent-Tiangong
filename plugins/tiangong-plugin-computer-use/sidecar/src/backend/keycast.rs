//! 按键可视化 HUD（keycast，macOS）：与 Windows `win_keycast` 同样式同时序。
//!
//! Agent 经 `desktop_input` 执行 key/combo 时，在主屏中下部显示按键卡片：
//! 左侧「天工」品牌徽标（靛紫，与天工虚拟指针一致），右侧每个按键渲染为
//! 独立键帽（`⌘` `⇧` `Z`）。`type` 文本输入不显示（内容可能敏感）。
//!
//! 连续按键时每一步各占一张卡片（共享 [`super::keycast_stack`] 逻辑）：新卡片
//! 出现在底部槽位，已有卡片平滑向上挤，每张卡片按自己的出现时间独立计时
//! 淡出；同一组键连按合并为 ×N 计数；同屏最多 5 张，溢出的最早卡片加速
//! 淡出——消除单窗口反复重绘造成的快速闪烁。
//!
//! 每张卡片一个窗口（淡出后回收复用），置顶、点击穿透、不抢焦点、所有
//! 桌面空间与全屏应用上可见，由 overlay 主循环（进程主线程）创建与驱动：
//! `show` 压栈并重建内容，`tick` 每帧推进淡入淡出与上挤动画。
use std::time::Instant;

use objc2::rc::{Allocated, Retained};
use objc2::{MainThreadMarker, class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSFontWeightBold, NSFontWeightSemibold,
    NSScreen, NSStatusWindowLevel, NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::keycast_stack::{Pushed, Stack};

/// HUD 底部槽位底边距屏幕可见区域底边的比例（中下部）。
const BOTTOM_RATIO: f64 = 0.18;

// ── 尺寸（points）──
/// 外层面板高度与内边距。
const PANEL_HEIGHT: f64 = 64.0;
const PANEL_PAD_X: f64 = 14.0;
const PANEL_RADIUS: f64 = 18.0;
/// 品牌徽标（「天工」胶囊）。
const BADGE_HEIGHT: f64 = 30.0;
const BADGE_PAD_X: f64 = 12.0;
const BADGE_FONT: f64 = 15.0;
/// 徽标与键帽区之间的间距。
const BADGE_GAP: f64 = 14.0;
/// 键帽。
const KEY_HEIGHT: f64 = 44.0;
const KEY_MIN_WIDTH: f64 = 44.0;
const KEY_PAD_X: f64 = 12.0;
const KEY_FONT: f64 = 24.0;
const KEY_RADIUS: f64 = 9.0;
const KEY_GAP: f64 = 6.0;
/// 重复计数（×3）。
const COUNT_FONT: f64 = 18.0;
const COUNT_GAP: f64 = 10.0;

/// 天工品牌主色（靛紫，与虚拟指针描边同色系）。
fn brand_color() -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(0.42, 0.36, 0.95, 1.0)
}

/// 创建透明背景、居中对齐的单行文字标签，返回 (标签, 实测文字尺寸)。
fn make_label(
    text: &str,
    font: &NSFont,
    color: &NSColor,
    mtm: MainThreadMarker,
) -> (Retained<NSTextField>, NSSize) {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(font));
    label.setTextColor(Some(color));
    label.setAlignment(NSTextAlignment::Center);
    label.sizeToFit();
    let size = label.frame().size;
    (label, size)
}

/// 创建圆角填充容器（NSBox Custom 类型，无标题、无内边距）。
fn make_box(
    frame: NSRect,
    fill: &NSColor,
    radius: f64,
    border: Option<&NSColor>,
) -> Retained<NSBox> {
    unsafe {
        // SAFETY：NSBox 类存在，alloc 后立即 initWithFrame。
        let allocated: Allocated<NSBox> = msg_send![class!(NSBox), alloc];
        let panel = NSBox::initWithFrame(allocated, frame);
        panel.setBoxType(NSBoxType::Custom);
        panel.setTitlePosition(NSTitlePosition::NoTitle);
        panel.setContentViewMargins(NSSize::new(0.0, 0.0));
        panel.setCornerRadius(radius);
        panel.setFillColor(fill);
        match border {
            Some(color) => {
                panel.setBorderWidth(1.0);
                panel.setBorderColor(color);
            }
            None => panel.setBorderWidth(0.0),
        }
        panel
    }
}

/// 把标签垂直居中放进 `(x, 容器高)` 处宽 `width` 的区域。
fn place_label(label: &NSTextField, text_size: NSSize, x: f64, width: f64, container_h: f64) {
    label.setFrame(NSRect::new(
        NSPoint::new(x, ((container_h - text_size.height) / 2.0).round()),
        NSSize::new(width, text_size.height),
    ));
}

/// 创建隐藏的卡片窗口（置顶、点击穿透、不抢焦点、全空间可见）。
fn create_card_window() -> Retained<NSWindow> {
    let frame = NSRect::new(NSPoint::new(-1000.0, -1000.0), NSSize::new(10.0, 10.0));
    unsafe {
        // SAFETY：NSWindow 类存在，alloc 后立即 init。
        let allocated: Allocated<NSWindow> = msg_send![class!(NSWindow), alloc];
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            allocated,
            frame,
            NSWindowStyleMask::empty(),
            NSBackingStoreType::Buffered,
            false,
        );
        window.setLevel(NSStatusWindowLevel);
        // 所有桌面空间可见、可覆盖全屏应用（VS Code 等全屏时仍能看到）。
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setIgnoresMouseEvents(true);
        window.setHasShadow(true);
        window.setAlphaValue(0.0);
        window.setReleasedWhenClosed(false);
        window
    }
}

/// 一张卡片的窗口资源。
struct CardWindow {
    id: u64,
    window: Retained<NSWindow>,
    width: f64,
}

/// 按键 HUD：卡片栈 + 每张卡片一个窗口（淡出后回收复用）。
pub struct KeyCastHud {
    stack: Stack,
    windows: Vec<CardWindow>,
    pool: Vec<Retained<NSWindow>>,
}

impl KeyCastHud {
    /// 创建 HUD（必须在主线程调用）。预建一个窗口确认可用。
    pub fn new(_mtm: MainThreadMarker) -> Option<Self> {
        Some(Self {
            stack: Stack::new(1.0),
            windows: Vec::new(),
            pool: vec![create_card_window()],
        })
    }

    /// 显示一组键帽：新卡片进入底部槽位，已有卡片向上挤；与最新卡片
    /// 相同的按键合并为 ×N 并重置其计时。
    pub fn show(&mut self, keys: &[String], mtm: MainThreadMarker) {
        if keys.is_empty() {
            return;
        }
        match self.stack.push(keys, PANEL_HEIGHT, Instant::now()) {
            Pushed::Merged(id) => {
                let Some(card) = self.stack.cards.iter().find(|c| c.id == id) else {
                    return;
                };
                let (content, width) = build_content(&card.keys, card.repeat, mtm);
                if let Some(slot) = self.windows.iter_mut().find(|w| w.id == id) {
                    slot.window.setContentView(Some(&content));
                    slot.width = width;
                }
            }
            Pushed::New(id) => {
                let (content, width) = build_content(keys, 1, mtm);
                let window = self.pool.pop().unwrap_or_else(create_card_window);
                window.setContentView(Some(&content));
                window.setAlphaValue(0.0);
                window.orderFrontRegardless();
                self.windows.push(CardWindow { id, window, width });
            }
        }
        self.present_all(mtm);
    }

    /// 每帧推进：各卡片独立淡入/淡出，上挤滑动，淡出完毕的窗口回收。
    pub fn tick(&mut self, mtm: MainThreadMarker) {
        if self.stack.cards.is_empty() {
            return;
        }
        for id in self.stack.advance(Instant::now()) {
            if let Some(index) = self.windows.iter().position(|w| w.id == id) {
                let slot = self.windows.remove(index);
                slot.window.setAlphaValue(0.0);
                slot.window.orderOut(None);
                self.pool.push(slot.window);
            }
        }
        self.present_all(mtm);
    }

    /// 按卡片当前偏移与透明度摆放全部窗口。
    fn present_all(&self, mtm: MainThreadMarker) {
        let Some(visible) = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame()) else {
            return;
        };
        for card in &self.stack.cards {
            let Some(slot) = self.windows.iter().find(|w| w.id == card.id) else {
                continue;
            };
            let base = hud_origin(visible, slot.width);
            // 栈偏移以屏幕向下为正；AppKit 原点在左下，向上为正，取反。
            let origin = NSPoint::new(base.x, (base.y - card.offset).round());
            slot.window.setFrame_display(
                NSRect::new(origin, NSSize::new(slot.width, PANEL_HEIGHT)),
                true,
            );
            slot.window.setAlphaValue(card.alpha);
        }
    }
}

/// 构建 HUD 内容：面板 [天工徽标 | 键帽… | ×N]，返回 (面板视图, 总宽)。
///
/// 先实测每个元素的文字宽度得到总宽，再按绝对坐标自左向右排布，
/// 面板宽度即窗口宽度，居中计算不受控件内边距影响。
fn build_content(keys: &[String], repeat: u32, mtm: MainThreadMarker) -> (Retained<NSView>, f64) {
    let white = NSColor::whiteColor();
    let badge_font = NSFont::systemFontOfSize_weight(BADGE_FONT, unsafe { NSFontWeightBold });
    let key_font = NSFont::systemFontOfSize_weight(KEY_FONT, unsafe { NSFontWeightSemibold });
    let count_font = NSFont::systemFontOfSize_weight(COUNT_FONT, unsafe { NSFontWeightSemibold });

    let (badge_label, badge_text) = make_label("天工", &badge_font, &white, mtm);
    let badge_width = badge_text.width + 2.0 * BADGE_PAD_X;
    let key_labels: Vec<(Retained<NSTextField>, NSSize, f64)> = keys
        .iter()
        .map(|k| {
            let (label, size) = make_label(k, &key_font, &white, mtm);
            let width = (size.width + 2.0 * KEY_PAD_X).max(KEY_MIN_WIDTH);
            (label, size, width)
        })
        .collect();
    let count = (repeat > 1).then(|| {
        make_label(
            &format!("×{repeat}"),
            &count_font,
            &NSColor::colorWithWhite_alpha(1.0, 0.75),
            mtm,
        )
    });

    let keys_width: f64 = key_labels.iter().map(|(_, _, w)| w).sum::<f64>()
        + KEY_GAP * (key_labels.len().saturating_sub(1)) as f64;
    let count_width = count
        .as_ref()
        .map(|(_, size)| COUNT_GAP + size.width)
        .unwrap_or(0.0);
    let total =
        (PANEL_PAD_X + badge_width + BADGE_GAP + keys_width + count_width + PANEL_PAD_X).ceil();

    let panel = make_box(
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(total, PANEL_HEIGHT)),
        &NSColor::colorWithWhite_alpha(0.07, 0.86),
        PANEL_RADIUS,
        Some(&NSColor::colorWithWhite_alpha(1.0, 0.12)),
    );

    // 天工徽标：品牌色胶囊 + 白字。
    let mut x = PANEL_PAD_X;
    let badge = make_box(
        NSRect::new(
            NSPoint::new(x, ((PANEL_HEIGHT - BADGE_HEIGHT) / 2.0).round()),
            NSSize::new(badge_width, BADGE_HEIGHT),
        ),
        &brand_color(),
        BADGE_HEIGHT / 2.0,
        None,
    );
    place_label(&badge_label, badge_text, 0.0, badge_width, BADGE_HEIGHT);
    badge.addSubview(&badge_label);
    panel.addSubview(&badge);
    x += badge_width + BADGE_GAP;

    // 键帽：浅色半透明圆角块 + 细描边。
    for (i, (label, size, width)) in key_labels.iter().enumerate() {
        if i > 0 {
            x += KEY_GAP;
        }
        let cap = make_box(
            NSRect::new(
                NSPoint::new(x, ((PANEL_HEIGHT - KEY_HEIGHT) / 2.0).round()),
                NSSize::new(*width, KEY_HEIGHT),
            ),
            &NSColor::colorWithWhite_alpha(1.0, 0.14),
            KEY_RADIUS,
            Some(&NSColor::colorWithWhite_alpha(1.0, 0.22)),
        );
        place_label(label, *size, 0.0, *width, KEY_HEIGHT);
        cap.addSubview(label);
        panel.addSubview(&cap);
        x += width;
    }

    // 重复计数。
    if let Some((label, size)) = count {
        x += COUNT_GAP;
        place_label(&label, size, x, size.width, PANEL_HEIGHT);
        panel.addSubview(&label);
    }

    (Retained::into_super(panel), total)
}

/// HUD 窗口原点（AppKit 坐标）：在可见区域内精确水平居中（取整到整点
/// 避免模糊），底边位于可见区域高度的 [`BOTTOM_RATIO`] 处（避开程序坞）。
fn hud_origin(visible: NSRect, width: f64) -> NSPoint {
    NSPoint::new(
        (visible.origin.x + (visible.size.width - width) / 2.0).round(),
        (visible.origin.y + visible.size.height * BOTTOM_RATIO).round(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hud_is_centered_in_lower_part_of_visible_frame() {
        let visible = NSRect::new(NSPoint::new(0.0, 80.0), NSSize::new(2560.0, 1360.0));
        let origin = hud_origin(visible, 200.0);
        // 窗口中心 = 屏幕中心。
        assert_eq!(origin.x + 100.0, 1280.0);
        assert_eq!(origin.y, (80.0 + 1360.0 * BOTTOM_RATIO).round());
        // 奇数宽度取整后偏差不超过半点。
        let origin = hud_origin(visible, 201.0);
        assert!((origin.x + 100.5 - 1280.0).abs() <= 0.5);
    }
}
