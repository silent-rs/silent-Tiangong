//! core 内置工具规格（仅 mark_step_completed / plugin_injection）与命令拆分辅助。
//!
//! web_fetch / run_command / run_shell 等工具的规格与执行已全部迁出至进程内插件
//! （fetch / command / browser / terminal），core 不再持有这些工具定义。

use crate::model::ToolSpec;

/// core 内置工具规格。
///
/// 仅保留 plan 执行控制（mark_step_completed）与插件注入通道（plugin_injection）。
/// 文件 / 网络 / 命令类工具均由进程内插件提供（fs / fetch / command / browser / terminal）。
pub(crate) fn basic_file_function_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "mark_step_completed".to_string(),
            description: "标记当前执行步骤已完成。仅在本步骤真正完成后调用。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "result": { "type": "string", "description": "本步骤完成结果摘要" },
                    "continue_execution": {
                        "type": "boolean",
                        "description": "当前 plan 是否需要继续执行后续动态步骤。true 表示需要追加下一步。"
                    },
                    "next_step_name": {
                        "type": "string",
                        "description": "当 continue_execution=true 时，下一步名称（必填）。"
                    },
                    "next_step_description": {
                        "type": "string",
                        "description": "当 continue_execution=true 时，下一步描述（必填）。"
                    }
                },
                "required": ["continue_execution"]
            }),
        },
        ToolSpec {
            name: "plugin_injection".to_string(),
            description: "插件单向注入通道。浏览器页面变化、终端用户操作等外部事件通过此工具自动注入对话。\n\n重要：你不需要主动调用此工具，它由系统在检测到外部变化时自动触发。注入的内容会以 tool result 形式出现在对话中，请据此理解用户环境和操作意图。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "数据来源（如 browser_data / terminal_user_input）" }
                },
                "required": []
            }),
        },
    ]
}

/// 拆分命令字符串为参数列表（支持引号、转义）。
pub(crate) fn split_command_parts(raw: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' if !in_single => {
                escaped = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped || in_single || in_double {
        return None;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() { None } else { Some(out) }
}

pub(crate) fn elapsed_ms_u64(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}
