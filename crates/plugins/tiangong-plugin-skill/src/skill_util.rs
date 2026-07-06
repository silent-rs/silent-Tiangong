pub(super) fn format_skill_record(
    status: &str,
    name: impl std::fmt::Display,
    detail: &str,
) -> String {
    format!("skills|{status}|name={name}|detail={detail}")
}
