//! Memory System 的 WASM 桥接组件。
//!
//! 本组件只做桥接：不承载 memory 纯逻辑（规划/提取/整理），不做内部编排。
//! 全部 memory 处理由 memory sidecar 完成（含 LLM/检索/存储的完整能力）。
//!
//! 工作方式：
//! - handle-tool(recall_memory) 时，经 memory-store.request host import
//!   把请求转发到 sidecar，结果原样返回。
//! - sidecar 不可用（无 handle）时，返回降级提示。
//!
//! 见 issue #321 / RFC docs/memory-system/11-memory-sidecar-wasm-bridge.md。

mod bindings;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::tiangong::plugin::memory_store;

mod descriptor {
    pub const ID: &str = "memory";
    pub const NAME: &str = "Memory";
    pub const VERSION: &str = "0.4.0";
}

/// recall_memory 工具的 input_schema（JSON 文本）。
/// 与进程内版本 `tiangong-plugin-memory/src/handler.rs` 保持一致。
const RECALL_MEMORY_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "要回忆的内容，结合用户当前请求改写成可检索查询"
    },
    "reason": {
      "type": "string",
      "description": "为什么需要回忆，简述当前任务依赖的历史语境"
    },
    "expected": {
      "type": "array",
      "items": { "type": "string" },
      "description": "期望找回的内容类型，如 media、file、tool_result、decision、code_context"
    },
    "limit": {
      "type": "integer",
      "description": "最多返回多少条记忆，默认 5，最大 10"
    }
  },
  "required": ["query"]
}"#;

const RECALL_MEMORY_DESCRIPTION: &str = "按需回忆历史上下文、跨会话结果、之前的工具输出或生成产物。用户提到刚刚、刚才、上次、之前、那个、继续、这张图、生成的图片等历史指代时，应先调用此工具。";

/// WASM 桥接组件（无状态）。
struct Component;

impl Guest for Component {
    fn describe() -> Result<PluginDescriptor, PluginError> {
        Ok(PluginDescriptor {
            id: descriptor::ID.to_string(),
            name: descriptor::NAME.to_string(),
            version: descriptor::VERSION.to_string(),
        })
    }

    fn tool_specs() -> Result<Vec<ToolSpec>, PluginError> {
        Ok(vec![ToolSpec {
            name: "recall_memory".to_string(),
            description: RECALL_MEMORY_DESCRIPTION.to_string(),
            input_schema: RECALL_MEMORY_INPUT_SCHEMA.to_string(),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        if call.name != "recall_memory" {
            return Err(PluginError::Message(format!(
                "memory 组件不支持工具: {}",
                call.name
            )));
        }

        // 桥接：把 recall_memory 的参数包成 MemoryRecallRequest，
        // 经 memory-store.request 转发到 sidecar 的 recall_context 完整编排。
        let request_payload = serde_json::json!({
            "method": "recall_context",
            "query": parse_query(&call.arguments).unwrap_or_default(),
            "reason": parse_string_field(&call.arguments, "reason"),
            "expected": parse_string_array(&call.arguments, "expected"),
            "context": [],
            "limit": parse_u32_field(&call.arguments, "limit").unwrap_or(5),
        });

        match memory_store::request("recall_context", &request_payload.to_string()) {
            Ok(response_json) => {
                // sidecar 返回的 MemoryIpcResponsePayload::RecallContext JSON，
                // 从中取 content 字段作为摘要。
                let content = serde_json::from_str::<serde_json::Value>(&response_json)
                    .ok()
                    .and_then(|v| v.get("content").and_then(|c| c.as_str()).map(String::from))
                    .unwrap_or_else(|| response_json.clone());
                Ok(tool_result_ok(content))
            }
            Err(memory_store::MemoryStoreError::Disabled) => Ok(tool_result_ok(
                "记忆系统未启用（memory sidecar 未连接）。".to_string(),
            )),
            Err(memory_store::MemoryStoreError::Message(m)) => {
                Ok(tool_result_ok(format!("记忆查询失败：{m}")))
            }
        }
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
        // 通用配置变更事件。桥接组件本身不消费配置，接收即可。
        Ok(())
    }
}

fn tool_result_ok(summary: String) -> ToolResult {
    ToolResult {
        ok: true,
        summary,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
    }
}

// ── 最小 JSON 解析（从工具参数中取字段） ──

fn parse_query(arguments: &str) -> Option<String> {
    parse_string_field(arguments, "query")
}

fn parse_string_field(arguments: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let idx = arguments.find(&key)?;
    let after_key = &arguments[idx + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = &after_key[colon + 1..];
    let quote = after_colon.find('"')?;
    let value_start = quote + 1;
    let value_rest = &after_colon[value_start..];
    let end_quote = value_rest.find('"')?;
    Some(value_rest[..end_quote].to_string())
}

fn parse_u32_field(arguments: &str, field: &str) -> Option<u32> {
    let key = format!("\"{field}\"");
    let idx = arguments.find(&key)?;
    let after_key = &arguments[idx + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = &after_key[colon + 1..];
    let digits: String = after_colon
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn parse_string_array(arguments: &str, field: &str) -> Vec<String> {
    let key = format!("\"{field}\"");
    let Some(idx) = arguments.find(&key) else {
        return Vec::new();
    };
    let after_key = &arguments[idx + key.len()..];
    let Some(open) = after_key.find('[') else {
        return Vec::new();
    };
    let rest = &after_key[open + 1..];
    let close = rest.find(']').unwrap_or(rest.len());
    let body = &rest[..close];
    let mut out = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == '"' {
            chars.next();
            let mut s = String::new();
            for cc in chars.by_ref() {
                if cc == '"' {
                    break;
                }
                s.push(cc);
            }
            if !s.is_empty() {
                out.push(s);
            }
        } else {
            chars.next();
        }
    }
    out
}

bindings::export!(Component with_types_in bindings);
