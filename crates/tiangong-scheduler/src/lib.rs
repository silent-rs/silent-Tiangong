use anyhow::{Result, bail};

pub mod executor;
pub mod model;
pub mod store;
pub mod webhook;

pub use executor::SchedulerContext;

pub(crate) const fn default_true() -> bool {
    true
}

/// 把多行/空白折叠为单行，并要求结果非空。
///
/// 定时任务与 webhook 的「任务名称 / 任务描述」均为展示为单行的字段，二者共用同一
/// 归一化规则，避免卡片渲染出现跨行错位。
pub(crate) fn normalize_required_single_line(field: &str, value: &str) -> Result<String> {
    let normalized = value
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    if normalized.is_empty() {
        bail!("{field}不能为空");
    }

    Ok(normalized)
}
