//! 键名归一化与按键 HUD 显示符号（平台无关纯函数）。
//!
//! macOS（CGEvent）与 Windows（SendInput）的键盘合成共用同一套键名：
//! 小写、去空白，接受常见别名。显示符号按平台习惯：macOS 用 `⌘⇧⌥⌃`
//! 标准符号并按 ⌃⌥⇧⌘ 排列；Windows 用 `Ctrl/Alt/Shift/Win` 文字并按
//! Win、Ctrl、Alt、Shift 排列。Windows 上 `cmd` 等同 `ctrl`（跨平台
//! 快捷键如 cmd+c 直接可用），Windows 徽标键用 `win`。

/// 键名归一化：小写、去空白、常见别名折算。
pub(crate) fn normalize_key_name(raw: &str) -> String {
    let trimmed = raw.trim().to_lowercase();
    let normalized = match trimmed.as_str() {
        "enter" => "return",
        "esc" => "escape",
        "backspace" => "delete",
        "del" => "forward_delete",
        "pageup" | "pgup" => "page_up",
        "pagedown" | "pgdn" => "page_down",
        "command" => "cmd",
        "super" | "win" | "windows" => "win",
        "control" => "ctrl",
        "option" | "opt" | "meta" => "alt",
        "right_cmd" | "rcmd" => "rcmd",
        "right_ctrl" | "rctrl" => "rctrl",
        "right_alt" | "roption" | "ralt" => "ralt",
        "right_shift" | "rshift" => "rshift",
        "arrow_left" | "left_arrow" | "arrowleft" => "left",
        "arrow_right" | "right_arrow" | "arrowright" => "right",
        "arrow_up" | "up_arrow" | "arrowup" => "up",
        "arrow_down" | "down_arrow" | "arrowdown" => "down",
        other => other,
    };
    normalized.to_string()
}

/// HUD 符号风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyStyle {
    Mac,
    Windows,
}

impl KeyStyle {
    fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Mac
        }
    }
}

/// 单键显示符号（当前平台风格）。
pub fn key_symbol(name: &str) -> String {
    symbol_for(name, KeyStyle::current())
}

/// 组合键 → 键帽序列（当前平台风格）：修饰键按平台标准顺序在前（同义
/// 修饰键去重），普通键在后。
pub fn combo_keys(names: &[String]) -> Vec<String> {
    combo_for(names, KeyStyle::current())
}

fn symbol_for(name: &str, style: KeyStyle) -> String {
    let symbol = match style {
        KeyStyle::Mac => match name {
            "cmd" | "rcmd" | "win" => "⌘",
            "shift" | "rshift" => "⇧",
            "alt" | "ralt" => "⌥",
            "ctrl" | "rctrl" => "⌃",
            "return" => "↩",
            "tab" => "⇥",
            "delete" => "⌫",
            "forward_delete" => "⌦",
            _ => return common_symbol(name),
        },
        KeyStyle::Windows => match name {
            "cmd" | "rcmd" | "ctrl" | "rctrl" => "Ctrl",
            "win" => "Win",
            "shift" | "rshift" => "Shift",
            "alt" | "ralt" => "Alt",
            "return" => "Enter",
            "tab" => "Tab",
            "delete" => "Backspace",
            "forward_delete" => "Del",
            "insert" => "Ins",
            _ => return common_symbol(name),
        },
    };
    symbol.to_string()
}

/// 两平台一致的键名显示。
fn common_symbol(name: &str) -> String {
    let symbol = match name {
        "space" => "Space",
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

/// 修饰键排序键（同一修饰键的左右/同义写法返回相同值，用于去重）。
fn modifier_rank(name: &str, style: KeyStyle) -> Option<u8> {
    match style {
        // macOS 菜单标准顺序：⌃ ⌥ ⇧ ⌘。
        KeyStyle::Mac => match name {
            "ctrl" | "rctrl" => Some(0),
            "alt" | "ralt" => Some(1),
            "shift" | "rshift" => Some(2),
            "cmd" | "rcmd" | "win" => Some(3),
            _ => None,
        },
        // Windows 文档习惯：Win + Ctrl + Alt + Shift（cmd 即 Ctrl）。
        KeyStyle::Windows => match name {
            "win" => Some(0),
            "cmd" | "rcmd" | "ctrl" | "rctrl" => Some(1),
            "alt" | "ralt" => Some(2),
            "shift" | "rshift" => Some(3),
            _ => None,
        },
    }
}

fn combo_for(names: &[String], style: KeyStyle) -> Vec<String> {
    let mut modifiers: Vec<&String> = names
        .iter()
        .filter(|n| modifier_rank(n, style).is_some())
        .collect();
    modifiers.sort_by_key(|n| modifier_rank(n, style));
    modifiers.dedup_by_key(|n| modifier_rank(n, style));
    modifiers
        .into_iter()
        .map(|n| symbol_for(n, style))
        .chain(
            names
                .iter()
                .filter(|n| modifier_rank(n, style).is_none())
                .map(|n| symbol_for(n, style)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalizes_common_aliases() {
        assert_eq!(normalize_key_name("Enter"), "return");
        assert_eq!(normalize_key_name(" ESC "), "escape");
        assert_eq!(normalize_key_name("Backspace"), "delete");
        assert_eq!(normalize_key_name("Del"), "forward_delete");
        assert_eq!(normalize_key_name("Command"), "cmd");
        assert_eq!(normalize_key_name("Option"), "alt");
        assert_eq!(normalize_key_name("Super"), "win");
        assert_eq!(normalize_key_name("PgDn"), "page_down");
        assert_eq!(normalize_key_name("ArrowLeft"), "left");
        assert_eq!(normalize_key_name(" A "), "a");
    }

    #[test]
    fn mac_symbols_and_order() {
        let style = KeyStyle::Mac;
        assert_eq!(symbol_for("down", style), "↓");
        assert_eq!(symbol_for("page_up", style), "PgUp");
        assert_eq!(symbol_for("return", style), "↩");
        assert_eq!(symbol_for("escape", style), "Esc");
        assert_eq!(symbol_for("c", style), "C");
        assert_eq!(symbol_for("f5", style), "F5");
        assert_eq!(combo_for(&names(&["cmd", "c"]), style), names(&["⌘", "C"]));
        // 输入顺序无关：统一按 ⌃⌥⇧⌘ 排列。
        assert_eq!(
            combo_for(&names(&["cmd", "shift", "z"]), style),
            names(&["⇧", "⌘", "Z"])
        );
        assert_eq!(
            combo_for(&names(&["ctrl", "alt", "cmd", "left"]), style),
            names(&["⌃", "⌥", "⌘", "←"])
        );
        // 左右修饰键同义不重复显示。
        assert_eq!(
            combo_for(&names(&["cmd", "rcmd", "v"]), style),
            names(&["⌘", "V"])
        );
    }

    #[test]
    fn windows_symbols_and_order() {
        let style = KeyStyle::Windows;
        assert_eq!(symbol_for("return", style), "Enter");
        assert_eq!(symbol_for("delete", style), "Backspace");
        assert_eq!(symbol_for("forward_delete", style), "Del");
        assert_eq!(symbol_for("left", style), "←");
        // cmd 即 Ctrl。
        assert_eq!(
            combo_for(&names(&["cmd", "c"]), style),
            names(&["Ctrl", "C"])
        );
        assert_eq!(
            combo_for(&names(&["shift", "ctrl", "escape"]), style),
            names(&["Ctrl", "Shift", "Esc"])
        );
        assert_eq!(
            combo_for(&names(&["shift", "win", "s"]), style),
            names(&["Win", "Shift", "S"])
        );
        // cmd 与 ctrl 同义去重。
        assert_eq!(
            combo_for(&names(&["cmd", "ctrl", "alt", "delete"]), style),
            names(&["Ctrl", "Alt", "Backspace"])
        );
    }
}
