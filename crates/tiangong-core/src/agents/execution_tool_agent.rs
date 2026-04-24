use anyhow::{Result, anyhow};

use crate::model::{FunctionToolSpec, ModelFunctionCall};
use crate::tool::{ToolCall, ToolName};

const INTERNAL_SHELL_CMD: &str = "__tiangong_shell__";
const INTERNAL_CWD_PREFIX: &str = "__tiangong_cwd=";
pub(crate) fn basic_file_function_tools() -> Vec<FunctionToolSpec> {
    vec![
        FunctionToolSpec {
            name: "list_dir".to_string(),
            description: "列出目录中的文件和子目录".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录路径，默认当前目录" }
                },
                "required": []
            }),
        },
        FunctionToolSpec {
            name: "tree_dir".to_string(),
            description: "按目录树格式列出目录，支持通过 max_depth 限制遍历深度".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录路径，默认当前目录" },
                    "max_depth": {
                        "type": "integer",
                        "description": "遍历最大深度，建议 1-4，默认 2，最大 8",
                        "minimum": 0,
                        "maximum": 8
                    }
                },
                "required": []
            }),
        },
        FunctionToolSpec {
            name: "read_file".to_string(),
            description: "读取文件内容，支持按行范围读取".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "start_line": { "type": "integer", "description": "起始行（从 1 开始，默认 1）", "minimum": 1 },
                    "max_lines": { "type": "integer", "description": "最大读取行数（默认 200，最大 2000）", "minimum": 1, "maximum": 2000 }
                },
                "required": ["path"]
            }),
        },
        FunctionToolSpec {
            name: "search_code".to_string(),
            description: "在目录中检索文本（优先使用 rg）".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "检索文本或正则模式" },
                    "path": { "type": "string", "description": "目标目录或文件路径，默认当前目录" }
                },
                "required": ["pattern"]
            }),
        },
        FunctionToolSpec {
            name: "write_file".to_string(),
            description: "写入文件内容（支持覆盖或追加）".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "content": { "type": "string", "description": "要写入的内容" },
                    "append": { "type": "boolean", "description": "是否追加写入，默认 false（覆盖）" }
                },
                "required": ["path", "content"]
            }),
        },
        FunctionToolSpec {
            name: "replace_in_file".to_string(),
            description: "在文件中将旧文本替换为新文本，默认仅允许单点替换".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "old": { "type": "string", "description": "待替换的旧文本" },
                    "new": { "type": "string", "description": "替换后的新文本" },
                    "replace_all": { "type": "boolean", "description": "是否替换全部命中，默认 false" },
                    "expected_count": { "type": "integer", "description": "预期命中数量（可选）", "minimum": 1 }
                },
                "required": ["path", "old", "new"]
            }),
        },
        FunctionToolSpec {
            name: "run_command".to_string(),
            description: "执行受控命令，支持 cwd 和超时设置。shell 脚本建议使用 run_shell"
                .to_string(),
            parameters: serde_json::json!({
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
        FunctionToolSpec {
            name: "run_shell".to_string(),
            description: "执行 shell 脚本，自动派生 bash/sh/powershell 参数".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "script": { "type": "string", "description": "shell 脚本文本" },
                    "shell": { "type": "string", "description": "shell 类型：auto/bash/sh/powershell/pwsh，默认 auto" },
                    "cwd": { "type": "string", "description": "工作目录（可选）" },
                    "timeout": { "type": "integer", "description": "超时时间（秒），0 或不填表示不限时", "minimum": 0 }
                },
                "required": ["script"]
            }),
        },
        FunctionToolSpec {
            name: "apply_patch".to_string(),
            description: "对文件应用补丁文本，仅支持 unified diff（---/+++/@@）".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "补丁内容文本（unified diff）" },
                    "verify": { "type": "boolean", "description": "是否仅校验不落盘（dry-run）" },
                    "workdir": { "type": "string", "description": "补丁工作目录（可选，默认当前工作目录）" }
                },
                "required": ["patch"]
            }),
        },
        FunctionToolSpec {
            name: "mark_step_completed".to_string(),
            description: "标记当前执行步骤已完成。仅在本步骤真正完成后调用。".to_string(),
            parameters: serde_json::json!({
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
    ]
}

pub(crate) fn build_tool_call_from_function(call: &ModelFunctionCall) -> Result<ToolCall> {
    if let Some(parse_error) = call
        .arguments
        .get("__parse_error")
        .and_then(serde_json::Value::as_str)
    {
        return Err(anyhow::anyhow!("{parse_error}"));
    }

    let mut args = Vec::new();
    let tool_call = match call.name.as_str() {
        "list_dir" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            args.push(path);
            Ok(ToolCall {
                name: ToolName::ListDir,
                args,
            })
        }
        "tree_dir" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            let max_depth = call
                .arguments
                .get("max_depth")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value.to_string())
                .or_else(|| {
                    call.arguments
                        .get("max_depth")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                })
                .unwrap_or_else(|| "2".to_string());
            args.push(path);
            args.push(max_depth);
            Ok(ToolCall {
                name: ToolName::TreeDir,
                args,
            })
        }
        "read_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(start_line) = call
                .arguments
                .get("start_line")
                .and_then(number_or_string_to_text)
            {
                args.push(path.clone());
                args.push(start_line);
                if let Some(max_lines) = call
                    .arguments
                    .get("max_lines")
                    .and_then(number_or_string_to_text)
                {
                    args.push(max_lines);
                }
                return Ok(ToolCall {
                    name: ToolName::ReadFile,
                    args,
                });
            }
            args.push(path);
            if let Some(max_lines) = call
                .arguments
                .get("max_lines")
                .and_then(number_or_string_to_text)
            {
                args.push("1".to_string());
                args.push(max_lines);
            }
            Ok(ToolCall {
                name: ToolName::ReadFile,
                args,
            })
        }
        "write_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let content = call
                .arguments
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(path);
            args.push(content);
            if let Some(append) = call
                .arguments
                .get("append")
                .and_then(bool_or_string_to_text)
            {
                args.push(append);
            }
            Ok(ToolCall {
                name: ToolName::WriteFile,
                args,
            })
        }
        "search_code" => {
            let pattern = call
                .arguments
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            args.push(pattern);
            args.push(path);
            Ok(ToolCall {
                name: ToolName::SearchCode,
                args,
            })
        }
        "replace_in_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let old = call
                .arguments
                .get("old")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let new = call
                .arguments
                .get("new")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(path);
            args.push(old);
            args.push(new);
            if let Some(replace_all) = call
                .arguments
                .get("replace_all")
                .and_then(bool_or_string_to_text)
            {
                args.push(replace_all);
            }
            if let Some(expected_count) = call
                .arguments
                .get("expected_count")
                .and_then(number_or_string_to_text)
            {
                if args.len() == 3 {
                    args.push("false".to_string());
                }
                args.push(expected_count);
            }
            Ok(ToolCall {
                name: ToolName::ReplaceInFile,
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
            Ok(ToolCall {
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
            Ok(ToolCall {
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
            Ok(ToolCall {
                name: ToolName::RunCommand,
                args,
            })
        }
        #[cfg(feature = "diffy")]
        "apply_patch" => {
            let patch = call
                .arguments
                .get("patch")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(patch);
            if let Some(verify) = call
                .arguments
                .get("verify")
                .and_then(bool_or_string_to_text)
            {
                args.push(verify);
            }
            if let Some(workdir) = call
                .arguments
                .get("workdir")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                if args.len() == 1 {
                    args.push("false".to_string());
                }
                args.push(workdir.to_string());
            }
            Ok(ToolCall {
                name: ToolName::ApplyPatch,
                args,
            })
        }
        #[cfg(not(feature = "diffy"))]
        "apply_patch" => Err(anyhow!(
            "apply_patch 工具未启用，请使用 --features diffy 编译"
        )),
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
