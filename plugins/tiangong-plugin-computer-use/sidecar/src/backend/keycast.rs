//! 按键可视化 HUD（keycast，仅 macOS）。
//!
//! Agent 经 `desktop_input` 执行 key/combo 时，在主屏中下部短暂显示按键
//! 符号（如 `⌘ C`、`↓`、`⇧⌘ Z`），约 1.2 秒后淡出，让用户看清 Agent 按了
//! 什么快捷键；`type` 文本输入不显示（内容可能敏感，且逐字显示无意义）。
//! 连续按同一键时显示重复次数（`↓  ×3`）。
//!
//! 窗口置顶、点击穿透、不抢焦点，由 overlay 主循环（进程主线程）创建与
//! 驱动：`show` 更新文本并定位，`tick` 每帧推进淡出。
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained};
use objc2::{MainThreadMarker, class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSScreen, NSStatusWindowLevel,
    NSTextAlignment, NSTextField, NSTitlePosition, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// 完全显示时长，之后开始淡出。
const HOLD: Duration = Duration::from_millis(1200);
/// 每帧（≈16ms）淡出步长，约 200ms 完全消失。
const FADE_STEP: f64 = 0.08;
const FONT_SIZE: f64 = 30.0;
const PAD_X: f64 = 26.0;
const PAD_Y: f64 = 12.0;
const CORNER_RADIUS: f64 = 14.0;
/// HUD 底边距屏幕可见区域底边的比例（中下部）。
const BOTTOM_RATIO: f64 = 0.18;

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

/// 组合键显示文本：修饰键按标准顺序紧凑拼接，空格后接普通键，
/// 如 `["shift","cmd","z"]` → `⇧⌘ Z`。
pub fn combo_label(names: &[String]) -> String {
    let mut modifiers: Vec<&String> = names
        .iter()
        .filter(|n| modifier_rank(n).is_some())
        .collect();
    modifiers.sort_by_key(|n| modifier_rank(n));
    modifiers.dedup_by_key(|n| modifier_rank(n));
    let mods: String = modifiers.iter().map(|n| key_symbol(n)).collect();
    let plains: Vec<String> = names
        .iter()
        .filter(|n| modifier_rank(n).is_none())
        .map(|n| key_symbol(n))
        .collect();
    match (mods.is_empty(), plains.is_empty()) {
        (true, _) => plains.join(" "),
        (false, true) => mods,
        (false, false) => format!("{mods} {}", plains.join(" ")),
    }
}

/// 按键 HUD 窗口。
pub struct KeyCastHud {
    window: Retained<NSWindow>,
    label: Retained<NSTextField>,
    last: String,
    repeat: u32,
    hide_at: Option<Instant>,
    alpha: f64,
}

impl KeyCastHud {
    /// 创建隐藏的 HUD 窗口（必须在主线程调用）。
    pub fn new(mtm: MainThreadMarker) -> Option<Self> {
        let frame = NSRect::new(NSPoint::new(-1000.0, -1000.0), NSSize::new(10.0, 10.0));
        unsafe {
            // SAFETY：NSWindow/NSBox 类存在，alloc 后立即 init。
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
            window.setReleasedWhenClosed(false);

            let allocated_box: Allocated<NSBox> = msg_send![class!(NSBox), alloc];
            let panel = NSBox::initWithFrame(allocated_box, frame);
            panel.setBoxType(NSBoxType::Custom);
            panel.setTitlePosition(NSTitlePosition::NoTitle);
            panel.setBorderWidth(0.0);
            panel.setCornerRadius(CORNER_RADIUS);
            panel.setFillColor(&NSColor::colorWithWhite_alpha(0.08, 0.78));
            panel.setContentViewMargins(NSSize::new(PAD_X, PAD_Y));

            let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
            label.setFont(Some(&NSFont::boldSystemFontOfSize(FONT_SIZE)));
            label.setTextColor(Some(&NSColor::whiteColor()));
            label.setAlignment(NSTextAlignment::Center);
            panel.setContentView(Some(&label));
            window.setContentView(Some(&panel));
            // 不抢 key/main 状态：用户当前操作零干扰。
            window.orderFrontRegardless();
            Some(Self {
                window,
                label,
                last: String::new(),
                repeat: 0,
                hide_at: None,
                alpha: 0.0,
            })
        }
    }

    /// 显示按键文本：居中于主屏可见区域中下部，重置淡出计时。
    pub fn show(&mut self, text: &str, mtm: MainThreadMarker) {
        let visible = self.hide_at.is_some();
        if visible && text == self.last {
            self.repeat += 1;
        } else {
            self.repeat = 1;
            self.last = text.to_string();
        }
        let display = if self.repeat > 1 {
            format!("{text}  ×{}", self.repeat)
        } else {
            text.to_string()
        };
        self.label.setStringValue(&NSString::from_str(&display));
        self.label.sizeToFit();
        let size = self.label.frame().size;
        let width = size.width + 2.0 * PAD_X;
        let height = size.height + 2.0 * PAD_Y;
        let origin = NSScreen::mainScreen(mtm)
            .map(|screen| hud_origin(screen.visibleFrame(), width))
            .unwrap_or_else(|| NSPoint::new(0.0, 0.0));
        self.window
            .setFrame_display(NSRect::new(origin, NSSize::new(width, height)), true);
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

/// HUD 窗口原点（AppKit 坐标）：水平居中，底边位于可见区域高度的
/// [`BOTTOM_RATIO`] 处（避开程序坞，位于屏幕中下部）。
fn hud_origin(visible: NSRect, width: f64) -> NSPoint {
    NSPoint::new(
        visible.origin.x + (visible.size.width - width) / 2.0,
        visible.origin.y + visible.size.height * BOTTOM_RATIO,
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
    fn combo_label_orders_modifiers_like_macos_menus() {
        assert_eq!(combo_label(&names(&["cmd", "c"])), "⌘ C");
        // 输入顺序无关：统一按 ⌃⌥⇧⌘ 排列。
        assert_eq!(combo_label(&names(&["cmd", "shift", "z"])), "⇧⌘ Z");
        assert_eq!(
            combo_label(&names(&["ctrl", "alt", "cmd", "left"])),
            "⌃⌥⌘ ←"
        );
        // 左右修饰键同义不重复显示。
        assert_eq!(combo_label(&names(&["cmd", "rcmd", "v"])), "⌘ V");
    }

    #[test]
    fn hud_is_centered_in_lower_part_of_visible_frame() {
        let visible = NSRect::new(NSPoint::new(0.0, 80.0), NSSize::new(2560.0, 1360.0));
        let origin = hud_origin(visible, 200.0);
        assert_eq!(origin.x, 1180.0);
        assert_eq!(origin.y, 80.0 + 1360.0 * BOTTOM_RATIO);
    }
}
