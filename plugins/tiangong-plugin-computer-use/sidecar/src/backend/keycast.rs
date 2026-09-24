//! 按键可视化 HUD（keycast，仅 macOS）。
//!
//! Agent 经 `desktop_input` 执行 key/combo 时，在主屏中下部短暂显示按键：
//! 左侧「天工」品牌徽标（靛紫渐变同色系，与天工虚拟指针一致），让用户
//! 一眼看出是天工在操作；右侧每个按键渲染为独立键帽（`⌘` `⇧` `Z`），
//! 连续按同一键时追加 `×3` 计数。约 1.2 秒后淡出。`type` 文本输入不显示
//! （内容可能敏感，且逐字显示无意义）。
//!
//! 布局全部由本模块按绝对坐标计算（各元素实测文字宽度后逐个排布），
//! 窗口宽度即内容总宽，保证在主屏可见区域内精确水平居中。
//!
//! 窗口置顶、点击穿透、不抢焦点、所有桌面空间与全屏应用上可见，由
//! overlay 主循环（进程主线程）创建与驱动：`show` 重建内容并定位，
//! `tick` 每帧推进淡出。
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained};
use objc2::{MainThreadMarker, class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSFontWeightBold, NSFontWeightSemibold,
    NSScreen, NSStatusWindowLevel, NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// 完全显示时长，之后开始淡出。
const HOLD: Duration = Duration::from_millis(1200);
/// 每帧（≈16ms）淡出步长，约 200ms 完全消失。
const FADE_STEP: f64 = 0.08;
/// HUD 底边距屏幕可见区域底边的比例（中下部）。
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

/// 单键显示符号。修饰键用 macOS 标准符号，特殊键用符号或短名，
/// 字母大写，其余原样。
pub fn key_symbol(name: &str) -> String {
    let symbol = match name {
        "cmd" | "rcmd" => "⌘",
        "shift" | "rshift" => "⇧",
        "alt" | "ralt" => "⌥",
        "ctrl" | "rctrl" => "⌃",
        "return" => "↩",
        "tab" => "⇥",
        "space" => "Space",
        "delete" => "⌫",
        "forward_delete" => "⌦",
        "escape" => "Esc",
        "left" => "←",
        "right" => "→",
        "up" => "↑",
        "down" => "↓",
        "home" => "Home",
        "end" => "End",
        "page_up" => "PgUp",
        "page_down" => "PgDn",
        "help" => "Help",
        other => return other.to_uppercase(),
    };
    symbol.to_string()
}

/// 修饰键在 macOS 菜单中的标准排列顺序：⌃ ⌥ ⇧ ⌘。
fn modifier_rank(name: &str) -> Option<u8> {
    match name {
        "ctrl" | "rctrl" => Some(0),
        "alt" | "ralt" => Some(1),
        "shift" | "rshift" => Some(2),
        "cmd" | "rcmd" => Some(3),
        _ => None,
    }
}

/// 组合键 → 键帽序列：修饰键按标准顺序在前（左右同义去重），普通键在后，
/// 如 `["cmd","shift","z"]` → `["⇧","⌘","Z"]`。
pub fn combo_keys(names: &[String]) -> Vec<String> {
    let mut modifiers: Vec<&String> = names
        .iter()
        .filter(|n| modifier_rank(n).is_some())
        .collect();
    modifiers.sort_by_key(|n| modifier_rank(n));
    modifiers.dedup_by_key(|n| modifier_rank(n));
    modifiers
        .into_iter()
        .map(|n| key_symbol(n))
        .chain(
            names
                .iter()
                .filter(|n| modifier_rank(n).is_none())
                .map(|n| key_symbol(n)),
        )
        .collect()
}

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

/// 按键 HUD 窗口。
pub struct KeyCastHud {
    window: Retained<NSWindow>,
    last: Vec<String>,
    repeat: u32,
    hide_at: Option<Instant>,
    alpha: f64,
}

impl KeyCastHud {
    /// 创建隐藏的 HUD 窗口（必须在主线程调用）。
    pub fn new(_mtm: MainThreadMarker) -> Option<Self> {
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
            // 不抢 key/main 状态：用户当前操作零干扰。
            window.orderFrontRegardless();
            Some(Self {
                window,
                last: Vec::new(),
                repeat: 0,
                hide_at: None,
                alpha: 0.0,
            })
        }
    }

    /// 显示一组键帽：重建内容视图，居中于主屏可见区域中下部，重置淡出计时。
    pub fn show(&mut self, keys: &[String], mtm: MainThreadMarker) {
        if keys.is_empty() {
            return;
        }
        let visible = self.hide_at.is_some();
        if visible && keys == self.last.as_slice() {
            self.repeat += 1;
        } else {
            self.repeat = 1;
            self.last = keys.to_vec();
        }
        let (content, width) = build_content(keys, self.repeat, mtm);
        self.window.setContentView(Some(&content));
        let origin = NSScreen::mainScreen(mtm)
            .map(|screen| hud_origin(screen.visibleFrame(), width))
            .unwrap_or_else(|| NSPoint::new(0.0, 0.0));
        self.window
            .setFrame_display(NSRect::new(origin, NSSize::new(width, PANEL_HEIGHT)), true);
        self.window.orderFrontRegardless();
        self.alpha = 1.0;
        self.window.setAlphaValue(1.0);
        self.hide_at = Some(Instant::now() + HOLD);
    }

    /// 每帧推进：超过显示时长后逐帧淡出。
    pub fn tick(&mut self) {
        if let Some(deadline) = self.hide_at
            && Instant::now() >= deadline
        {
            self.alpha = (self.alpha - FADE_STEP).max(0.0);
            self.window.setAlphaValue(self.alpha);
            if self.alpha == 0.0 {
                self.hide_at = None;
            }
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

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn key_symbol_maps_special_keys_and_uppercases_letters() {
        assert_eq!(key_symbol("down"), "↓");
        assert_eq!(key_symbol("page_up"), "PgUp");
        assert_eq!(key_symbol("return"), "↩");
        assert_eq!(key_symbol("escape"), "Esc");
        assert_eq!(key_symbol("c"), "C");
        assert_eq!(key_symbol("f5"), "F5");
    }

    #[test]
    fn combo_keys_orders_modifiers_like_macos_menus() {
        assert_eq!(combo_keys(&names(&["cmd", "c"])), names(&["⌘", "C"]));
        // 输入顺序无关：统一按 ⌃⌥⇧⌘ 排列。
        assert_eq!(
            combo_keys(&names(&["cmd", "shift", "z"])),
            names(&["⇧", "⌘", "Z"])
        );
        assert_eq!(
            combo_keys(&names(&["ctrl", "alt", "cmd", "left"])),
            names(&["⌃", "⌥", "⌘", "←"])
        );
        // 左右修饰键同义不重复显示。
        assert_eq!(
            combo_keys(&names(&["cmd", "rcmd", "v"])),
            names(&["⌘", "V"])
        );
    }

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
