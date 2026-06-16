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
    "terminal_session_recent_output",
    "terminal_session_info",
    "terminal_session_set_cwd",
    "terminal_session_resize",
    "terminal_session_reset",
    "terminal_panel_set_session",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
