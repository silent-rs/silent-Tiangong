//! Memory System 的 WASM 组件示例（阶段一 PoC）。
//!
//! 本组件是 issue #321 阶段一的验证载体：
//! - 导出 `describe` / `tool-specs` / `prompt-sections` / `handle-tool` / `shutdown`；
//! - 承载 `recall_memory` 工具，以**纯 mock 数据**返回，不访问任何存储或模型；
//! - 用于验证宿主侧 `tiangong-plugin-runtime` 的加载、调用与资源限制链路。
//!
//! 后续阶段（纯逻辑下沉 / 存储 host import）会在此组件内逐步填充真实逻辑。

mod bindings;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};

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

/// WASM 组件主体。阶段一为无状态组件。
struct Component;

impl Guest for Component {
    fn describe() -> Result<PluginDescriptor, PluginError> {
        Ok(PluginDescriptor {
            id: "memory".to_string(),
            name: "Memory".to_string(),
            version: "0.1.0".to_string(),
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
        // 阶段一 PoC 不注入 prompt 段落。
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        if call.name != "recall_memory" {
            return Err(PluginError::Message(format!(
                "memory 组件不支持工具: {}",
                call.name
            )));
        }

        // 阶段一：纯 mock 返回。从 arguments JSON 中取出 query 展示在摘要里。
        let query = parse_query(&call.arguments).unwrap_or_else(|| "(空查询)".to_string());
        let summary = format!(
            "[mock recall_memory] 已为你回忆与「{query}」相关的历史上下文（PoC 占位结果）。"
        );

        Ok(ToolResult {
            ok: true,
            summary,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        })
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }
}

/// 从 arguments（JSON 文本）中解析 query 字段。
/// 阶段一不引入 serde，用最小手写解析避免额外依赖。
fn parse_query(arguments: &str) -> Option<String> {
    // 寻找 "query" 键对应的字符串值。容错处理简单 JSON。
    let key = "\"query\"";
    let idx = arguments.find(key)?;
    let after_key = &arguments[idx + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = &after_key[colon + 1..];
    let quote = after_colon.find('"')?;
    let value_start = quote + 1;
    let value_rest = &after_colon[value_start..];
    let end_quote = value_rest.find('"')?;
    Some(value_rest[..end_quote].to_string())
}

bindings::export!(Component with_types_in bindings);
