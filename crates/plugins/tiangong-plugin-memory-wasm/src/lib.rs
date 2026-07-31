//! Memory System 的 WASM 桥接组件。
//!
//! 本组件只做桥接：不承载 memory 纯逻辑（规划/提取/整理），不做内部编排。
//! 全部 memory 处理由 memory sidecar 完成（含 LLM/检索/存储的完整能力）。
//!
//! 工作方式：
//! - handle-tool(recall_memory) 时，经通用 sidecar host import
//!   把请求转发到 sidecar，结果原样返回。
//! - sidecar 不可用时，返回明确提示。
//!
//! 见 issue #321 / RFC docs/memory-system/11-memory-sidecar-wasm-bridge.md。

mod bindings;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use bindings::tiangong::plugin::sidecar;

/// 向绑定的 sidecar 发送 Memory 业务操作；通用协议由运行时封装。
fn sidecar_call(
    operation: &str,
    payload: &serde_json::Value,
) -> Result<String, sidecar::SidecarError> {
    let encoded = serde_json::to_string(payload)
        .map_err(|error| sidecar::SidecarError::Message(format!("序列化请求失败: {error}")))?;
    sidecar::invoke(operation, &encoded)
}

mod descriptor {
    pub const ID: &str = "memory";
    pub const NAME: &str = "Memory";
    pub const VERSION: &str = "0.6.0";
}

/// 全局状态缓存（WASM 单线程，RefCell 安全）。
/// 存放 prompt_sections 拉注入所需的 session_id 和 workspace。
mod state {
    use std::cell::RefCell;

    struct PluginState {
        session_id: Option<String>,
        workspace: Option<String>,
    }

    thread_local! {
        static STATE: RefCell<PluginState> = const { RefCell::new(PluginState {
            session_id: None,
            workspace: None,
        }) };
    }

    pub fn set_session_id(id: Option<String>) {
        STATE.with(|s| s.borrow_mut().session_id = id);
    }

    pub fn set_workspace(ws: Option<String>) {
        STATE.with(|s| s.borrow_mut().workspace = ws);
    }

    pub fn session_id() -> Option<String> {
        STATE.with(|s| s.borrow().session_id.clone())
    }

    /// workspace_id = workspace 路径的末尾目录名（与原生 memory 一致）。
    pub fn workspace_id() -> Option<String> {
        STATE.with(|s| {
            s.borrow().workspace.as_ref().and_then(|ws| {
                ws.rsplit('/')
                    .next()
                    .filter(|n| !n.is_empty())
                    .map(String::from)
            })
        })
    }
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

const MEMORY_PAGE_TEMPLATE: &str = include_str!("memory.html");
const MEMORY_PAGE_CSS: &str = include_str!("memory.css");
const MEMORY_PAGE_JS: &str = include_str!("memory.js");

fn memory_settings_html() -> String {
    MEMORY_PAGE_TEMPLATE
        .replace("/*__MEMORY_CSS__*/", MEMORY_PAGE_CSS)
        .replace("/*__MEMORY_JS__*/", MEMORY_PAGE_JS)
}

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
        // 读缓存的状态，经 request 拉取三级记忆注入。
        let session_id = state::session_id().unwrap_or_default();
        let workspace_id = state::workspace_id();
        let payload = serde_json::json!({
            "session_id": session_id,
            "workspace_id": workspace_id,
        });
        let sections = match sidecar_call("load_injection", &payload) {
            Ok(response_json) => {
                // sidecar 返回 MemoryIpcResponsePayload::Injection { items: Vec<String> }。
                serde_json::from_str::<serde_json::Value>(&response_json)
                    .ok()
                    .and_then(|v| v.get("items").cloned())
                    .and_then(|items| serde_json::from_value(items).ok())
                    .unwrap_or_default()
            }
            Err(_) => {
                // sidecar 不可用时返回空（不注入），不阻断 prompt 装配。
                Vec::new()
            }
        };
        Ok(sections)
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        if call.name != "recall_memory" {
            return Err(PluginError::Message(format!(
                "memory 组件不支持工具: {}",
                call.name
            )));
        }

        // 桥接：把 recall_memory 的参数包成 MemoryRecallRequest，
        // 经通用 sidecar 接口转发到 recall_context 完整编排。
        let request_payload = serde_json::json!({
            "request": {
                "query": parse_query(&call.arguments).unwrap_or_default(),
                "reason": parse_string_field(&call.arguments, "reason"),
                "expected": parse_string_array(&call.arguments, "expected"),
                "context": [],
                "limit": parse_u32_field(&call.arguments, "limit").unwrap_or(5),
            }
        });

        match sidecar_call("recall_context", &request_payload) {
            Ok(response_json) => {
                // sidecar 返回的 MemoryIpcResponsePayload::RecallContext JSON，
                // 从中取 content 字段作为摘要。
                let content = serde_json::from_str::<serde_json::Value>(&response_json)
                    .ok()
                    .and_then(|v| {
                        v.get("response")
                            .and_then(|response| response.get("content"))
                            .and_then(|content| content.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| response_json.clone());
                Ok(tool_result_ok(content))
            }
            Err(sidecar::SidecarError::Unavailable) => Ok(tool_result_ok(
                "记忆系统未启用（memory sidecar 未连接）。".to_string(),
            )),
            Err(sidecar::SidecarError::Message(m)) => {
                Ok(tool_result_ok(format!("记忆查询失败：{m}")))
            }
        }
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(workspace: Option<String>) -> Result<(), PluginError> {
        state::set_workspace(workspace);
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
        // 通用配置变更事件。桥接组件本身不消费配置，接收即可。
        Ok(())
    }

    // ── 生命周期钩子 ──
    //
    // session-json 为宿主 Session 的只读快照（可序列化部分）。
    // WASM 从中提取 memory 需要的数据，经 request 转发到 sidecar。
    // session 的所有修改权始终在 Core，WASM/ sidecar 绝不回写。

    fn on_session_ready(session_json: String) -> Result<(), PluginError> {
        // 会话就绪：从 session 快照提取 id 缓存，供 prompt_sections 拉注入用。
        if let Ok(session) = serde_json::from_str::<serde_json::Value>(&session_json) {
            let id = session.get("id").and_then(|v| v.as_str()).map(String::from);
            state::set_session_id(id);
        }
        Ok(())
    }

    fn on_turn_started(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        // 轮次开始：当前无需通知 sidecar。
        Ok(())
    }

    fn on_turn_finished(session_json: String, turn_start_idx: u32) -> Result<(), PluginError> {
        // 轮次结束：从 session 只读快照提取本轮信息，转发给 sidecar 做 micro 反刍。
        // 提取失败（session 格式异常）仅记录，不阻断——反刍是 best-effort。
        let _ = forward_turn_rumination(&session_json, turn_start_idx);
        Ok(())
    }

    fn on_session_ended(session_json: String) -> Result<(), PluginError> {
        // 会话结束：从 session 提取 id/cwd，转发给 sidecar 做 meso 反刍。
        let _ = forward_session_rumination(&session_json);
        Ok(())
    }
}

// ── UI 能力（plugin-ui 接口），独立于 Core 使用的 plugin 接口 ──

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(vec![Contribution {
            id: "memory".to_string(),
            title: "记忆".to_string(),
            description: "记忆系统配置（模型端点、向量检索等）".to_string(),
            icon: "brain".to_string(),
            group: "plugins".to_string(),
            has_view: true,
        }])
    }

    fn open_view(contribution_id: String) -> Result<ViewResponse, PluginError> {
        if contribution_id != "memory" {
            return Err(PluginError::Message(format!(
                "未知的 contribution: {contribution_id}"
            )));
        }
        Ok(ViewResponse {
            html: memory_settings_html(),
        })
    }

    fn get_view_resource(path: String) -> Result<ResourceResponse, PluginError> {
        match path.as_str() {
            "memory.css" => Ok(ResourceResponse {
                data: MEMORY_PAGE_CSS.as_bytes().to_vec(),
                mime: "text/css; charset=utf-8".to_string(),
            }),
            "memory.js" => Ok(ResourceResponse {
                data: MEMORY_PAGE_JS.as_bytes().to_vec(),
                mime: "text/javascript; charset=utf-8".to_string(),
            }),
            _ => Err(PluginError::Message(format!("无此资源: {path}"))),
        }
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        let payload = match request.method.as_str() {
            "bootstrap" => request_memory_host("ui.memory.config.get", "{}"),
            "save_config" => request_memory_host("ui.memory.config.set", &request.payload),
            "memory_request" => forward_memory_ui_request(&request.payload),
            other => Err(PluginError::Message(format!("未知消息: {other}"))),
        }?;
        Ok(ViewMessageResponse { payload })
    }
}

fn request_memory_host(method: &str, payload: &str) -> Result<String, PluginError> {
    let payload_val =
        serde_json::from_str::<serde_json::Value>(payload).unwrap_or(serde_json::Value::Null);
    sidecar_call(method, &payload_val).map_err(|error| match error {
        sidecar::SidecarError::Unavailable => PluginError::Message("Memory 未启用".to_string()),
        sidecar::SidecarError::Message(message) => PluginError::Message(message),
    })
}

fn forward_memory_ui_request(payload: &str) -> Result<String, PluginError> {
    let method = serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("method")
                .and_then(|method| method.as_str())
                .map(String::from)
        })
        .ok_or_else(|| PluginError::Message("Memory 页面请求缺少 method".to_string()))?;
    match method.as_str() {
        "list_nodes"
        | "count_nodes"
        | "list_relations"
        | "list_relations_batch"
        | "upsert_manual_memory"
        | "set_node_status"
        | "upsert_relation"
        | "delete_relation"
        | "recall" => request_memory_host("ui.memory.request", payload),
        _ => Err(PluginError::Message(format!(
            "Memory 页面不支持请求: {method}"
        ))),
    }
}

/// 从 session 快照提取本轮信息，转发给 sidecar 做 enhanced micro 反刍。
///
/// 提取 session.id、本轮 user_input、工具调用名，组装成 EnhancedTurnResult 的
/// 简化形式，经 request("run_enhanced_micro_rumination", ...) 转发。
fn forward_turn_rumination(session_json: &str, turn_start_idx: u32) -> Result<(), PluginError> {
    let session: serde_json::Value = serde_json::from_str(session_json)
        .map_err(|e| PluginError::Message(format!("解析 session 失败: {e}")))?;
    let session_id = session
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let messages = session.get("messages").cloned().unwrap_or_default();
    let idx = turn_start_idx as usize;

    // 提取本轮 user_input（messages[idx] 的文本内容）。
    let user_input = messages
        .get(idx)
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    // 提取本轮工具调用名列表。
    let tool_calls: Vec<String> = messages
        .as_array()
        .map(|arr| {
            arr.iter()
                .skip(idx)
                .filter_map(|m| {
                    let is_tool = m
                        .get("role")
                        .and_then(|r| r.as_str())
                        .map(|r| r == "tool")
                        .unwrap_or(false);
                    if is_tool {
                        m.get("tool_name")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // 组装 EnhancedTurnResult 的简化 payload，转发到 sidecar。
    let payload = serde_json::json!({
        "turn_result": {
            "session_id": session_id,
            "turn_id": format!("turn-{turn_start_idx}"),
            "had_tool_calls": !tool_calls.is_empty(),
            "user_input": user_input.clone(),
            "summary": user_input,
            "tool_calls": tool_calls,
            "artifacts": [],
            "workspace_id": state::workspace_id(),
            "memory_candidates": [],
            "turn_messages": [],
        }
    });
    // sidecar 可能不可用（disabled），反刍是 best-effort，忽略错误。
    let _ = sidecar_call("run_enhanced_micro_rumination", &payload);
    Ok(())
}

/// 从 session 快照提取 id/cwd，转发给 sidecar 做 meso 反刍。
fn forward_session_rumination(session_json: &str) -> Result<(), PluginError> {
    let session: serde_json::Value = serde_json::from_str(session_json)
        .map_err(|e| PluginError::Message(format!("解析 session 失败: {e}")))?;
    let session_id = session
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let workspace_id = session
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let payload = serde_json::json!({
        "session_id": session_id,
        "workspace_id": workspace_id,
    });
    let _ = sidecar_call("run_meso_rumination", &payload);
    Ok(())
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
