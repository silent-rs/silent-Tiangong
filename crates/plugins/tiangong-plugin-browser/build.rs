const COMMANDS: &[&str] = &[
    "browser_open",
    "browser_close",
    "browser_hide",
    "browser_set_position",
    "browser_navigate",
    "browser_eval",
    "browser_go_back",
    "browser_go_forward",
    "browser_reload",
    "browser_tab_list",
    "browser_snapshot_tabs",
    "browser_switch_session",
    "browser_tab_new",
    "browser_tab_switch",
    "browser_tab_close",
    "browser_annotation_extract",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
