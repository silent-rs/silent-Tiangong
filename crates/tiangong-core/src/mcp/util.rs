pub(super) fn format_mcp_record(status: &str, server: &str, detail: &str) -> String {
    format!("mcp|{status}|server={server}|detail={detail}")
}

pub(super) fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut out = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}
