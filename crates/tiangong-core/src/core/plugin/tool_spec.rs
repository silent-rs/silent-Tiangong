//! 插件基础设施相关的工具规格定义集中点。
//!
//! 把散落在各处的插件注入通道工具名常量集中管理，避免重复字面量。
//! `INJECTION_TOOL_NAME` 的权威定义在 [`crate::react::message`]，这里仅做别名
//! re-export，供插件子系统内部引用。

use crate::react::message::INJECTION_TOOL_NAME;

/// 插件事件注入通道的工具名（单一来源别名，供本子系统内部引用）。
///
/// 值同 [`crate::react::message::INJECTION_TOOL_NAME`]，即 `"plugin_injection"`。
pub(crate) const INJECTION_TOOL: &str = INJECTION_TOOL_NAME;
