use anyhow::{Result, anyhow};

use crate::model::{ToolCall, ToolSpec};
use crate::tool::{ToolCall as LocalToolCall, ToolName};

const INTERNAL_SHELL_CMD: &str = "__tiangong_shell__";
const INTERNAL_CWD_PREFIX: &str = "__tiangong_cwd=";
pub(crate) fn basic_file_function_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "web_fetch".to_string(),
            description: "使用内嵌浏览器获取 URL 内容。支持 HTTP/HTTPS 网页和本地 file:// 或绝对路径的 HTML 文件。当用户要求在浏览器中打开页面或预览 HTML 时必须使用此工具，不要使用 run_shell 调用系统浏览器。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "要获取的 HTTP/HTTPS URL" },
                    "mode": {
                        "type": "string",
                        "enum": ["text", "download"],
                        "description": "执行模式，默认 text"
                    },
                    "max_chars": {
                        "type": "integer",
                        "description": "text 模式最多返回字符数，默认 12000，最大 50000",
                        "minimum": 1,
                        "maximum": 50000
                    },
                    "output_path": {
                        "type": "string",
                        "description": "download 模式目标文件路径，必须位于允许写入目录"
                    },
                    "overwrite": {
                        "type": "boolean",
                        "description": "download 模式是否覆盖已有文件，默认 false"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "请求超时时间，默认 15000，最大 60000",
                        "minimum": 1000,
                        "maximum": 60000
                    },
                    "follow_redirects": {
                        "type": "boolean",
                        "description": "是否跟随重定向，默认 true"
                    },
                    "extract_mode": {
                        "type": "string",
                        "enum": ["auto", "text", "raw"],
                        "description": "text 模式提取方式，默认 auto"
                    }
                },
                "required": ["url"]
            }),
        },
        ToolSpec {
            name: "run_command".to_string(),
            description: "执行受控命令，支持 cwd 和超时设置。shell 脚本建议使用 run_shell"
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "命令名" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "命令参数列表"
                    },
                    "cwd": { "type": "string", "description": "工作目录（可选）" },
                    "timeout": { "type": "integer", "description": "超时时间（秒），0 或不填表示不限时", "minimum": 0 }
                },
                "required": ["cmd"]
            }),
        },
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

pub(crate) fn build_tool_call_from_function(call: &ToolCall) -> Result<LocalToolCall> {
    if let Some(parse_error) = call
        .arguments
        .get("__parse_error")
        .and_then(serde_json::Value::as_str)
    {
        return Err(anyhow::anyhow!("{parse_error}"));
    }

    let mut args = Vec::new();
    let tool_call = match call.name.as_str() {
        "web_fetch" => {
            let url = call
                .arguments
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mode = call
                .arguments
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("text")
                .to_string();
            let max_chars = call
                .arguments
                .get("max_chars")
                .and_then(number_or_string_to_text)
                .unwrap_or_else(|| "12000".to_string());
            let output_path = call
                .arguments
                .get("output_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let overwrite = call
                .arguments
                .get("overwrite")
                .and_then(bool_or_string_to_text)
                .unwrap_or_else(|| "false".to_string());
            let timeout_ms = call
                .arguments
                .get("timeout_ms")
                .and_then(number_or_string_to_text)
                .unwrap_or_else(|| "15000".to_string());
            let follow_redirects = call
                .arguments
                .get("follow_redirects")
                .and_then(bool_or_string_to_text)
                .unwrap_or_else(|| "true".to_string());
            let extract_mode = call
                .arguments
                .get("extract_mode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("auto")
                .to_string();
            args.extend([
                url,
                mode,
                max_chars,
                output_path,
                overwrite,
                timeout_ms,
                follow_redirects,
                extract_mode,
            ]);
            Ok(LocalToolCall {
                name: ToolName::WebFetch,
                args,
            })
        }
        "run_command" => {
            let raw_cmd = call
                .arguments
                .get("cmd")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if let Some(mut parts) = split_command_parts(&raw_cmd)
                && !parts.is_empty()
            {
                args.push(parts.remove(0));
                args.extend(parts);
            }
            if args.is_empty() {
                args.push(raw_cmd);
            }
            if let Some(arr) = call
                .arguments
                .get("args")
                .and_then(serde_json::Value::as_array)
            {
                args.extend(
                    arr.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(ToString::to_string),
                );
            }
            if let Some(cwd) = call
                .arguments
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                args.push(format!("{INTERNAL_CWD_PREFIX}{cwd}"));
            }
            if let Some(timeout) = call
                .arguments
                .get("timeout")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .filter(|v| *v > 0)
            {
                args.push(format!("__tiangong_timeout={}", timeout * 1000));
            }
            Ok(LocalToolCall {
                name: ToolName::RunCommand,
                args,
            })
        }
        "run_shell" => {
            let script = call
                .arguments
                .get("script")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let shell = call
                .arguments
                .get("shell")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("auto")
                .to_string();
            args.push(INTERNAL_SHELL_CMD.to_string());
            args.push(script);
            args.push(shell);
            if let Some(cwd) = call
                .arguments
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                args.push(format!("{INTERNAL_CWD_PREFIX}{cwd}"));
            }
            if let Some(timeout) = call
                .arguments
                .get("timeout")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .filter(|v| *v > 0)
            {
                args.push(format!("__tiangong_timeout={}", timeout * 1000));
            }
            Ok(LocalToolCall {
                name: ToolName::RunCommand,
                args,
            })
        }
        "run_bash" => {
            let script = call
                .arguments
                .get("script")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(INTERNAL_SHELL_CMD.to_string());
            args.push(script);
            args.push("bash".to_string());
            Ok(LocalToolCall {
                name: ToolName::RunCommand,
                args,
            })
        }
        _ => Err(anyhow!("未知函数调用：{}", call.name)),
    }?;
    Ok(tool_call)
}

fn number_or_string_to_text(value: &serde_json::Value) -> Option<String> {
    value
        .as_u64()
        .map(|v| v.to_string())
        .or_else(|| value.as_i64().map(|v| v.to_string()))
        .or_else(|| value.as_str().map(ToString::to_string))
}

fn bool_or_string_to_text(value: &serde_json::Value) -> Option<String> {
    value
        .as_bool()
        .map(|v| v.to_string())
        .or_else(|| value.as_str().map(ToString::to_string))
}

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
