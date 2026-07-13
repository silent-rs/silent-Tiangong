use serde_json::json;
use tiangong_core::model::ToolSpec;
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};

use crate::constants::*;

pub(crate) fn root_tool_specs() -> Vec<ToolSpec> {
    let mut specs = child_tool_specs();
    specs.splice(
        0..0,
        [
            ToolSpec {
                name: TOOL_CREATE_AGENT.to_string(),
                description: "创建一个由独立 TiangongCore 承载的团队成员。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "role": {
                            "type": "string",
                            "description": "稳定的 @提及标识，只能含字母、数字、下划线；不区分大小写且不能使用系统保留字"
                        },
                        "label": { "type": "string", "description": "显示名称" },
                        "system_prompt": { "type": "string", "description": "角色职责与行为要求" }
                    },
                    "required": ["role", "label", "system_prompt"]
                }),
            },
            ToolSpec {
                name: TOOL_DISMISS_AGENT.to_string(),
                description: "关闭并解散指定团队成员，等待其独立 Core 完整退出。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "role": { "type": "string", "description": "要解散的 Agent 角色" }
                    },
                    "required": ["role"]
                }),
            },
        ],
    );
    specs
}

pub(crate) fn child_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: TOOL_SEND_MESSAGE.to_string(),
            description: "向指定 Agent 发送消息并等待其完整轮次；发给 main 时仅异步投递。"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "目标 Agent role，支持 @role 或 main" },
                    "content": { "type": "string", "description": "消息内容" },
                    "priority": { "type": "string", "enum": ["normal", "urgent"] }
                },
                "required": ["to", "content"]
            }),
        },
        ToolSpec {
            name: TOOL_BROADCAST_MESSAGE.to_string(),
            description: "向所有允许形成无环等待边的 Agent 广播消息并等待完成。".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string" },
                    "exclude": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["content"]
            }),
        },
        ToolSpec {
            name: TOOL_NOTIFY_USER.to_string(),
            description: "直接向用户推送带 Agent 身份的通知。".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string" },
                    "level": { "type": "string", "enum": ["info", "warning", "error", "question"] }
                },
                "required": ["content"]
            }),
        },
        ToolSpec {
            name: TOOL_LOCK_FILE.to_string(),
            description: "获取文件或目录编辑锁；Sub Agent 写入前必须调用。".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: TOOL_UNLOCK_FILE.to_string(),
            description: "释放当前 Agent 持有的文件或目录编辑锁。".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        },
    ]
}

pub(crate) fn ok_result(
    tool_name: &str,
    summary: impl Into<String>,
    stdout: impl Into<String>,
    args: Vec<String>,
) -> ToolResult {
    let summary = summary.into();
    ToolResult {
        ok: true,
        summary: summary.clone(),
        stdout: stdout.into(),
        stderr: String::new(),
        exit_code: 0,
        execution: Some(ToolExecutionRecord {
            tool_name: tool_name.to_string(),
            args,
            duration_ms: 0,
            ok: true,
            exit_code: 0,
            summary,
        }),
    }
}

pub(crate) fn error_result(tool_name: &str, message: impl Into<String>) -> ToolResult {
    let message = message.into();
    ToolResult {
        ok: false,
        summary: format!("{tool_name} 失败：{message}"),
        stdout: String::new(),
        stderr: message,
        exit_code: 1,
        execution: Some(ToolExecutionRecord {
            tool_name: tool_name.to_string(),
            args: Vec::new(),
            duration_ms: 0,
            ok: false,
            exit_code: 1,
            summary: format!("{tool_name} 失败"),
        }),
    }
}
