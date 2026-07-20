const COMMANDS: &[&str] = &[
    "terminal_exec",
    "terminal_recent_output",
    "terminal_send_input",
    "terminal_reset",
    "terminal_system_session_info",
    "terminal_set_cwd",
    "terminal_resize",
    "terminal_ensure_session",
    "terminal_destroy_session",
    "terminal_session_send_input",
    "terminal_report_user_command",
    "terminal_session_recent_output",
    "terminal_session_info",
    "terminal_session_set_cwd",
    "terminal_session_resize",
    "terminal_session_reset",
    "terminal_session_update_screen",
    "terminal_panel_set_session",
    "terminal_tab_list",
    "terminal_tab_new",
    "terminal_tab_restore",
    "terminal_tab_switch",
    "terminal_tab_close",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
