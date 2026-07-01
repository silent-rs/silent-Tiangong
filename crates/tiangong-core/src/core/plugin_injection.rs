//! 插件事件注入通道（plugin_injection）的工具规格。
//!
//! `plugin_injection` 是 core 的插件事件协议：浏览器页面变化、终端用户操作等外部事件
//! 通过此 synthetic tool 以 tool result 形式注入对话。它不是某个具体进程内插件的能力
//! 工具，而是所有插件共享的注入通道，因此归属 core plugin 基础设施。
//!
//! tool_call name 复用 [`crate::react::message::INJECTION_TOOL_NAME`]，避免重复定义。

use crate::model::ToolSpec;
use crate::react::message::INJECTION_TOOL_NAME;

/// 插件注入通道的工具规格。
///
/// 描述里明确告知模型「不要主动调用」——该工具由系统在检测到外部变化时自动触发。
pub(crate) fn tool_spec() -> ToolSpec {
    ToolSpec {
        name: INJECTION_TOOL_NAME.to_string(),
        description: "插件单向注入通道。浏览器页面变化、终端用户操作等外部事件通过此工具自动注入对话。\n\n重要：你不需要主动调用此工具，它由系统在检测到外部变化时自动触发。注入的内容会以 tool result 形式出现在对话中，请据此理解用户环境和操作意图。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "source": { "type": "string", "description": "数据来源（如 browser_data / terminal_user_input）" }
            },
            "required": []
        }),
    }
}
