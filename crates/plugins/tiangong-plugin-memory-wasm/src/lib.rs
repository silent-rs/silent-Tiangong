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
    Contribution, Guest, PluginDescriptor, PluginError, ResourceResponse, ToolCall, ToolResult,
    ToolSpec, ViewMessageRequest, ViewMessageResponse, ViewResponse,
};
use bindings::tiangong::plugin::memory_store;

mod descriptor {
    pub const ID: &str = "memory";
    pub const NAME: &str = "Memory";
    pub const VERSION: &str = "0.5.0";
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
        static STATE: RefCell<PluginState> = RefCell::new(PluginState {
            session_id: None,
            workspace: None,
        });
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

/// memory 设置页的完整 HTML（内联 CSS + JS，单文件嵌入 WASM）。
/// 页面经 postMessage 与天工通信，读写配置经 handle-view-message。
const MEMORY_SETTINGS_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>记忆配置</title>
<style>
  body { font-family: system-ui, sans-serif; padding: 16px; margin: 0; color: #1a1a1a; background: #fff; }
  h2 { margin: 0 0 16px; font-size: 18px; }
  .field { margin-bottom: 14px; }
  label { display: block; font-size: 13px; font-weight: 500; margin-bottom: 4px; }
  input { width: 100%; padding: 8px 10px; border: 1px solid #d0d0d0; border-radius: 6px; font-size: 13px; box-sizing: border-box; }
  input:focus { outline: none; border-color: #6366f1; }
  .help { font-size: 11px; color: #888; margin-top: 3px; }
  button { padding: 8px 18px; background: #6366f1; color: #fff; border: none; border-radius: 6px; font-size: 13px; cursor: pointer; }
  button:hover { background: #5558e3; }
  button:disabled { opacity: 0.5; cursor: default; }
  .saved { color: #16a34a; font-size: 12px; margin-left: 10px; }
</style>
</head>
<body>
<h2>记忆系统配置</h2>
<div class="field">
  <label>记忆 LLM 模型 key</label>
  <input id="model_key" placeholder="留空则用规则 fallback">
  <div class="help">主模型配置中的模型 key（留空则不走 LLM，用规则提取）</div>
</div>
<div class="field">
  <label>向量模型 key</label>
  <input id="embedding_key" placeholder="留空则只用关键词检索">
  <div class="help">语义检索用的 embedding 模型 key</div>
</div>
<div class="field">
  <label>重排模型 key</label>
  <input id="rerank_key" placeholder="留空则不重排">
  <div class="help">召回结果重排模型 key</div>
</div>
<div class="field">
  <label>向量模式</label>
  <input id="vector_mode" placeholder="auto">
  <div class="help">auto / disabled / embedded_lancedb</div>
</div>
<div style="margin-top: 20px;">
  <button id="save" onclick="save()">保存</button>
  <span id="saved" class="saved" style="display:none">已保存</span>
</div>
<script>
// 初始化：加载现有配置
async function init() {
  try {
    const config = await callHost('get_config', '');
    const values = JSON.parse(config || '{}');
    for (const [key, input] of Object.entries({
      model_key: 'model_key', embedding_key: 'embedding_key',
      rerank_key: 'rerank_key', vector_mode: 'vector_mode'
    })) {
      const el = document.getElementById(input);
      if (el && values[key] !== undefined) el.value = values[key];
    }
  } catch(e) { console.error('加载配置失败', e); }
}

async function save() {
  const btn = document.getElementById('save');
  btn.disabled = true;
  const config = JSON.stringify({
    model_key: document.getElementById('model_key').value,
    embedding_key: document.getElementById('embedding_key').value,
    rerank_key: document.getElementById('rerank_key').value,
    vector_mode: document.getElementById('vector_mode').value
  });
  try {
    await callHost('set_config', config);
    const saved = document.getElementById('saved');
    saved.style.display = 'inline';
    setTimeout(() => saved.style.display = 'none', 2000);
  } catch(e) { console.error('保存失败', e); }
  btn.disabled = false;
}

// 经 postMessage 与天工通信
function callHost(method, payload) {
  return new Promise((resolve, reject) => {
    const id = Math.random().toString(36);
    const handler = (e) => {
      if (e.data && e.data.id === id) {
        window.removeEventListener('message', handler);
        if (e.data.error) reject(new Error(e.data.error));
        else resolve(e.data.result);
      }
    };
    window.addEventListener('message', handler);
    window.parent.postMessage({ type: 'plugin_call', id, method, payload }, '*');
  });
}

init();
</script>
</body>
</html>"#;

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
            "method": "load_injection",
            "session_id": session_id,
            "workspace_id": workspace_id,
        });
        let sections = match memory_store::request("load_injection", &payload.to_string()) {
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

    // ── UI 贡献：设置页 ──

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
            html: MEMORY_SETTINGS_HTML.to_string(),
        })
    }

    fn get_view_resource(path: String) -> Result<ResourceResponse, PluginError> {
        // memory 设置页是单文件内联 HTML，无额外资源。
        // 如需 CSS/JS/图标分离，可在此按 path 返回对应资源。
        Err(PluginError::Message(format!("无此资源: {path}")))
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        match request.method.as_str() {
            "get_config" => {
                // 经 WASI filesystem 读取自己的配置。
                let content = std::fs::read_to_string("config.json").unwrap_or_default();
                Ok(ViewMessageResponse { payload: content })
            }
            "set_config" => {
                // 经 WASI filesystem 写入自己的配置。
                std::fs::write("config.json", &request.payload)
                    .map_err(|e| PluginError::Message(format!("写入配置失败: {e}")))?;
                Ok(ViewMessageResponse {
                    payload: "ok".to_string(),
                })
            }
            other => Err(PluginError::Message(format!("未知消息: {other}"))),
        }
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
        "method": "run_enhanced_micro_rumination",
        "session_id": session_id,
        "user_input": user_input,
        "tool_calls": tool_calls,
    });
    // sidecar 可能不可用（disabled），反刍是 best-effort，忽略错误。
    let _ = memory_store::request("run_enhanced_micro_rumination", &payload.to_string());
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
        "method": "run_meso_rumination",
        "session_id": session_id,
        "workspace_id": workspace_id,
    });
    let _ = memory_store::request("run_meso_rumination", &payload.to_string());
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
