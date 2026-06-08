use anyhow::{Result, anyhow};

use crate::model::{ToolCall, ToolSpec};
use crate::tool::{ToolCall as LocalToolCall, ToolName};

const INTERNAL_SHELL_CMD: &str = "__tiangong_shell__";
const INTERNAL_CWD_PREFIX: &str = "__tiangong_cwd=";
pub(crate) fn basic_file_function_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_dir".to_string(),
            description: "列出目录中的文件和子目录".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录路径，默认当前目录" }
                },
                "required": []
            }),
        },
        ToolSpec {
            name: "tree_dir".to_string(),
            description: "按目录树格式列出目录，支持通过 max_depth 限制遍历深度".to_string(),
            input_schema: serde_json::json!({
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
        ToolSpec {
            name: "read_file".to_string(),
            description: "读取文件内容，支持按行范围读取".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "start_line": { "type": "integer", "description": "起始行（从 1 开始，默认 1）", "minimum": 1 },
                    "max_lines": { "type": "integer", "description": "最大读取行数（默认 200，最大 2000）", "minimum": 1, "maximum": 2000 }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "search_code".to_string(),
            description: "在目录中检索文本（优先使用 rg）".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "检索文本或正则模式" },
                    "path": { "type": "string", "description": "目标目录或文件路径，默认当前目录" }
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "current_time".to_string(),
            description: "获取当前本地时间、RFC3339 时间、Unix 时间戳和时区偏移。涉及今天、现在、当前时间、日期换算等请求时使用。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolSpec {
            name: "scheduler".to_string(),
            description: "管理定时任务（Cron Job）。支持创建、列出、更新、删除定时任务，查看执行历史。用户可以通过对话要求设置定时提醒、周期性任务等。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create_job", "list_jobs", "update_job", "delete_job", "trigger_job", "get_job_runs"],
                        "description": "操作类型"
                    },
                    "name": { "type": "string", "description": "任务名称（create_job 必填，update_job 可选）" },
                    "description": { "type": "string", "description": "任务描述（create_job 必填，update_job 可选）" },
                    "schedule": { "type": "string", "description": "Cron 表达式，如 '0 9 * * *' 表示每天 9 点（create_job 必填，update_job 可选）" },
                    "payload": { "type": "string", "description": "触发时发送给 LLM 的任务描述（create_job 必填，update_job 可选）" },
                    "session_id": { "type": "string", "description": "关联已有会话 ID（可选，不指定则自动创建新会话）" },
                    "id": { "type": "string", "description": "任务 ID（update_job/delete_job/trigger_job/get_job_runs 必填）" },
                    "enabled": { "type": "boolean", "description": "是否启用（update_job 可选）" },
                    "limit": { "type": "integer", "description": "返回记录数量，默认 10（get_job_runs 可选）" }
                },
                "required": ["action"]
            }),
        },
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
            name: "web_form_extract".to_string(),
            description: "提取天工内嵌浏览器当前页面中所有表单的字段信息。支持原生 HTML 表单和 UI 库自定义组件（Ant Design Select/DatePicker、Element Plus Select/DatePicker 等）。返回结构化的字段列表（含框架检测信息），供 web_form_fill 填写。当需要帮用户填写网页表单时，先调用此工具了解页面有哪些可填写字段。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolSpec {
            name: "web_form_fill".to_string(),
            description: "在天工内嵌浏览器当前页面中填写指定表单字段。支持原生 HTML 控件（input/select/textarea）和 UI 库自定义组件（Ant Design Select、Element Plus Select、DatePicker 等）。selector 参数可传 CSS 选择器，也可传自然语言定位描述；支持 text=、role=、aria=、label=、placeholder=、name= 以及“邮箱输入框”这类描述。填写前建议先调用 web_form_extract；如果工具返回候选列表，下一次调用要使用候选中的 selector 或更精确描述。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "字段定位描述。可用 CSS selector，也可用自然语言或 DSL，例如：邮箱输入框、label=邮箱、placeholder=请输入邮箱、name=email、role=textbox[name=邮箱]" },
                    "value": { "type": "string", "description": "要填写的值" },
                    "strategy": { "type": "string", "enum": ["auto", "native", "keyboard", "paste"], "description": "填写策略，默认 auto（自动选择最佳策略）" }
                },
                "required": ["selector", "value"]
            }),
        },
        ToolSpec {
            name: "web_click".to_string(),
            description: "点击天工内嵌浏览器当前页面中的指定元素（按钮、链接等）。selector 参数可传 CSS 选择器，也可传自然语言定位描述；支持 text=、role=button[name=登录]、aria=关闭、label=提交、placeholder=搜索，以及“登录按钮”“表格第三行第二列的链接”等描述。元素会先滚动到可视区域再点击；如果工具返回候选列表，下一次调用要使用候选中的 selector 或更精确描述。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "点击目标定位描述。可用 CSS selector，也可用自然语言或 DSL，例如：登录按钮、text=提交、role=button[name=登录]、aria=关闭、表格第三行第二列的链接" }
                },
                "required": ["selector"]
            }),
        },
        ToolSpec {
            name: "web_locate_element".to_string(),
            description: "在天工内嵌浏览器当前页面中定位元素，不执行任何操作，仅返回匹配结果。用于在 click 或 fill 之前探测元素是否存在以及有多少候选。query 参数支持 CSS 选择器和自然语言定位描述（text=、role=、label=、placeholder= 等）。返回结果包含最佳匹配（target）和候选列表（candidates），供后续 web_click 或 web_form_fill 使用更精确的 selector。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "元素定位描述。可用 CSS selector，也可用自然语言或 DSL，例如：登录按钮、text=提交、role=button[name=登录]、label=邮箱" }
                },
                "required": ["query"]
            }),
        },
        ToolSpec {
            name: "write_file".to_string(),
            description: "写入文件内容（支持覆盖或追加）".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "content": { "type": "string", "description": "要写入的内容" },
                    "append": { "type": "boolean", "description": "是否追加写入，默认 false（覆盖）" }
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: "replace_in_file".to_string(),
            description: "在文件中将旧文本替换为新文本，默认仅允许单点替换".to_string(),
            input_schema: serde_json::json!({
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
            name: "run_shell".to_string(),
            description: "执行 shell 脚本，自动派生 bash/sh/powershell 参数".to_string(),
            input_schema: serde_json::json!({
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
        ToolSpec {
            name: "apply_patch".to_string(),
            description: "对文件应用补丁文本，仅支持 unified diff（---/+++/@@）".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "补丁内容文本（unified diff）" },
                    "verify": { "type": "boolean", "description": "是否仅校验不落盘（dry-run）" },
                    "workdir": { "type": "string", "description": "补丁工作目录（可选，默认当前工作目录）" }
                },
                "required": ["patch"]
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
        "list_dir" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            args.push(path);
            Ok(LocalToolCall {
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
            Ok(LocalToolCall {
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
                return Ok(LocalToolCall {
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
            Ok(LocalToolCall {
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
            Ok(LocalToolCall {
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
            Ok(LocalToolCall {
                name: ToolName::SearchCode,
                args,
            })
        }
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
        "current_time" => Ok(LocalToolCall {
            name: ToolName::CurrentTime,
            args,
        }),
        "scheduler" => {
            let action = call
                .arguments
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let get_str = |key: &str| -> String {
                call.arguments
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let get_opt_str = |key: &str| -> String {
                call.arguments
                    .get(key)
                    .and_then(|v| if v.is_null() { None } else { v.as_str() })
                    .unwrap_or("")
                    .to_string()
            };
            args.push(action);
            args.push(get_str("name"));
            args.push(get_str("description"));
            args.push(get_str("schedule"));
            args.push(get_str("payload"));
            args.push(get_opt_str("session_id"));
            args.push(get_opt_str("enabled"));
            args.push(get_str("id"));
            args.push(get_str("limit"));
            Ok(LocalToolCall {
                name: ToolName::Scheduler,
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
            Ok(LocalToolCall {
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
            Ok(LocalToolCall {
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
