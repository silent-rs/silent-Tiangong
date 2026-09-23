//! 坐标级键盘合成输入（RFC 0018 §2.4，仅 macOS）：CGEvent 键盘事件。
//!
//! 与 desktop_mouse 同属 CGEvent 合成输入族：键盘事件投递给当前焦点
//! 应用（与真人一致），不影响系统鼠标，无借用归还问题。输入框焦点
//! 由 Agent 先用 desktop_mouse click 建立。
//!
//! 三个手势而非裸事件：
//! - `type`：逐字符 Unicode 字符串分派（`CGEventKeyboardSetUnicodeString`），
//!   不经输入法组字、不依赖物理布局，中文/表情与 ASCII 同路径；
//! - `key`：单键名 → 虚拟键码（HIToolbox ANSI 表，US 布局）；
//! - `combo`：修饰键 down → 普通键 down/up → 修饰键逆序 up 的真实
//!   HID 序列（部分应用监听修饰键本身，与点击手势同一真实管线路由）。
//!
//! 键名大小写不敏感，接受常见别名（enter→return、esc→escape、
//! backspace→delete、del→forward_delete 等）。
use std::thread::sleep;
use std::time::Duration;

use core_graphics::event::{CGEvent, CGEventTapLocation, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use tiangong_plugin_computer_use_protocol::ops::KeyboardActionKind;

/// key 手势 down → up 间隔。
const KEY_DOWN_UP: Duration = Duration::from_millis(50);
/// type 手势相邻字符间隔。
const TYPE_GAP: Duration = Duration::from_millis(15);
/// combo 修饰键相邻事件间隔。
const MOD_GAP: Duration = Duration::from_millis(20);

struct KeyboardIo {
    source: CGEventSource,
}

impl KeyboardIo {
    fn new() -> Result<Self, String> {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|()| "创建 CGEventSource 失败".to_string())?;
        Ok(Self { source })
    }
    fn post_key(&self, keycode: CGKeyCode, key_down: bool) -> Result<(), String> {
        let event = CGEvent::new_keyboard_event(self.source.clone(), keycode, key_down)
            .map_err(|()| "创建键盘事件失败".to_string())?;
        event.post(CGEventTapLocation::HID);
        Ok(())
    }
    /// 投递一个 Unicode 字符：down/up 成对携带同一字符串（部分应用只读
    /// keyDown 的字符串，成对最稳）。虚拟键码为 0（字符串分派不依赖布局）。
    fn post_char(&self, ch: &str, key_down: bool) -> Result<(), String> {
        let event = CGEvent::new_keyboard_event(self.source.clone(), 0, key_down)
            .map_err(|()| "创建键盘事件失败".to_string())?;
        event.set_string(ch);
        event.post(CGEventTapLocation::HID);
        Ok(())
    }
}

/// 键名归一化：小写、去空白、常见别名折算。
pub(crate) fn normalize_key_name(raw: &str) -> String {
    let trimmed = raw.trim().to_lowercase();
    match trimmed.as_str() {
        "enter" => "return".to_string(),
        "esc" => "escape".to_string(),
        "backspace" => "delete".to_string(),
        "del" => "forward_delete".to_string(),
        "pageup" => "page_up".to_string(),
        "pagedown" => "page_down".to_string(),
        "command" | "super" | "win" => "cmd".to_string(),
        "control" => "ctrl".to_string(),
        "option" | "opt" | "meta" => "alt".to_string(),
        "right_cmd" | "rcmd" => "rcmd".to_string(),
        "right_ctrl" | "rctrl" => "rctrl".to_string(),
        "right_alt" | "roption" | "ralt" => "ralt".to_string(),
        "right_shift" | "rshift" => "rshift".to_string(),
        "arrow_left" | "left_arrow" | "arrowleft" => "left".to_string(),
        "arrow_right" | "right_arrow" | "arrowright" => "right".to_string(),
        "arrow_up" | "up_arrow" | "arrowup" => "up".to_string(),
        "arrow_down" | "down_arrow" | "arrowdown" => "down".to_string(),
        other => other.to_string(),
    }
}

/// 修饰键虚拟键码（HIToolbox：cmd=55 rcmd=54 shift=56 rshift=60
/// alt=58 ralt=61 ctrl=59 rctrl=62）。
fn modifier_keycode(name: &str) -> Option<CGKeyCode> {
    Some(match name {
        "cmd" => 55,
        "rcmd" => 54,
        "shift" => 56,
        "rshift" => 60,
        "alt" => 58,
        "ralt" => 61,
        "ctrl" => 59,
        "rctrl" => 62,
        _ => return None,
    })
}

/// 非修饰键虚拟键码（HIToolbox Events.h ANSI 表，US 布局）。
fn plain_keycode(name: &str) -> Option<CGKeyCode> {
    Some(match name {
        "return" => 36,
        "tab" => 48,
        "space" => 49,
        "delete" => 51,
        "forward_delete" => 117,
        "escape" => 53,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        "home" => 115,
        "end" => 119,
        "page_up" => 116,
        "page_down" => 121,
        "help" => 114,
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        "a" => 0x00,
        "s" => 0x01,
        "d" => 0x02,
        "f" => 0x03,
        "h" => 0x04,
        "g" => 0x05,
        "z" => 0x06,
        "x" => 0x07,
        "c" => 0x08,
        "v" => 0x09,
        "b" => 0x0B,
        "q" => 0x0C,
        "w" => 0x0D,
        "e" => 0x0E,
        "r" => 0x0F,
        "y" => 0x10,
        "t" => 0x11,
        "1" => 0x12,
        "2" => 0x13,
        "3" => 0x14,
        "4" => 0x15,
        "6" => 0x16,
        "5" => 0x17,
        "9" => 0x19,
        "7" => 0x1A,
        "8" => 0x1C,
        "0" => 0x1D,
        "o" => 0x1F,
        "u" => 0x20,
        "i" => 0x22,
        "p" => 0x23,
        "l" => 0x25,
        "j" => 0x26,
        "k" => 0x28,
        "n" => 0x2D,
        "m" => 0x2E,
        _ => return None,
    })
}

/// 校验键名（wasm 只做结构校验，键名合法性由 perform 单点裁决）。
#[cfg(test)]
pub(crate) fn is_known_key(name: &str) -> bool {
    let normalized = normalize_key_name(name);
    modifier_keycode(&normalized).is_some() || plain_keycode(&normalized).is_some()
}

/// 执行一次键盘手势；成功返回人读摘要。
pub fn perform(
    action: KeyboardActionKind,
    text: Option<String>,
    key: Option<String>,
    keys: Option<Vec<String>>,
) -> Result<String, String> {
    let io = KeyboardIo::new()?;
    match action {
        KeyboardActionKind::Type => {
            let text = text
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or("type 缺少 text")?;
            for ch in text.chars() {
                let unit = ch.to_string();
                io.post_char(&unit, true)?;
                sleep(KEY_DOWN_UP);
                io.post_char(&unit, false)?;
                sleep(TYPE_GAP);
            }
            Ok(format!("已输入 {len} 个字符", len = text.chars().count()))
        }
        KeyboardActionKind::Key => {
            let name = key
                .as_deref()
                .map(normalize_key_name)
                .filter(|n| !n.is_empty())
                .ok_or("key 缺少键名")?;
            let keycode = plain_keycode(&name)
                .ok_or_else(|| format!("不支持的键名: {name}（修饰键请用 combo）"))?;
            io.post_key(keycode, true)?;
            sleep(KEY_DOWN_UP);
            io.post_key(keycode, false)?;
            Ok(format!("已按 {name}"))
        }
        KeyboardActionKind::Combo => {
            let keys = keys.ok_or("combo 缺少 keys")?;
            let names: Vec<String> = keys
                .iter()
                .map(|k| normalize_key_name(k))
                .filter(|n| !n.is_empty())
                .collect();
            if names.len() < 2 {
                return Err("combo 至少需要两个键（修饰键 + 普通键）".to_string());
            }
            let modifiers: Vec<&String> = names
                .iter()
                .filter(|n| modifier_keycode(n).is_some())
                .collect();
            let plains: Vec<&String> = names
                .iter()
                .filter(|n| modifier_keycode(n).is_none())
                .collect();
            if plains.len() != 1 {
                return Err(format!(
                    "combo 需要恰好一个非修饰键（当前 {} 个）；单独按修饰键没有意义",
                    plains.len()
                ));
            }
            let plain_name = plains[0].clone();
            let plain = plain_keycode(&plain_name)
                .ok_or_else(|| format!("combo 不支持的键名: {plain_name}"))?;
            // 修饰键 down（保持给定顺序）→ 普通键 down/up → 修饰键逆序 up。
            for m in &modifiers {
                io.post_key(modifier_keycode(m).expect("已过滤为修饰键"), true)?;
                sleep(MOD_GAP);
            }
            io.post_key(plain, true)?;
            sleep(KEY_DOWN_UP);
            io.post_key(plain, false)?;
            for m in modifiers.iter().rev() {
                sleep(MOD_GAP);
                io.post_key(modifier_keycode(m).expect("已过滤为修饰键"), false)?;
            }
            Ok(format!("已执行组合键 {}", names.join("+")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_aliases() {
        assert_eq!(normalize_key_name("Enter"), "return");
        assert_eq!(normalize_key_name(" ESC "), "escape");
        assert_eq!(normalize_key_name("Backspace"), "delete");
        assert_eq!(normalize_key_name("Del"), "forward_delete");
        assert_eq!(normalize_key_name("Command"), "cmd");
        assert_eq!(normalize_key_name("Option"), "alt");
        assert_eq!(normalize_key_name("ArrowLeft"), "left");
        assert_eq!(normalize_key_name(" A "), "a");
    }

    #[test]
    fn known_keys_cover_letters_digits_and_specials() {
        for k in ["a", "Z", "0", "9", "return", "esc", "f5", "up"] {
            assert!(is_known_key(k), "{k} 应识别");
        }
        for k in ["cmd", "ctrl", "shift", "alt", "rcmd"] {
            assert!(is_known_key(k), "{k} 应识别为修饰键");
        }
        assert!(!is_known_key("hup"), "未知键应拒绝");
        assert!(!is_known_key(""), "空键名应拒绝");
        assert!(!is_known_key("fn"), "fn 不是可合成键");
    }

    #[test]
    fn plain_and_modifier_keycodes_disjoint() {
        // 修饰键不得同时出现在普通键表（combo 依赖两表互斥分区）。
        for m in [
            "cmd", "rcmd", "shift", "rshift", "alt", "ralt", "ctrl", "rctrl",
        ] {
            assert!(modifier_keycode(m).is_some());
            assert!(plain_keycode(m).is_none(), "{m} 不应视为普通键");
        }
        for p in ["return", "space", "a", "f12"] {
            assert!(plain_keycode(p).is_some());
            assert!(modifier_keycode(p).is_none());
        }
    }
}
