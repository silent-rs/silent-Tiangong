const COMMANDS: &[&str] = &[
    "browser_open",
    "browser_close",
    "browser_hide",
    "browser_set_position",
    "browser_navigate",
    "browser_eval",
    "browser_go_back",
    "browser_go_forward",
    "browser_tab_list",
    "browser_tab_new",
    "browser_tab_switch",
    "browser_tab_close",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
