//! Subagent 插件的 WASM 桥接组件。
//!
//! 职责（其余全部在 sidecar 进程内）：
//! - 工具规格声明与透传执行（sidecar 返回统一 ToolOutcome 形状）；
//! - `on_turn_finished` 生命周期钩子：提取本轮用户消息与最终回复，转发给
//!   sidecar 完成「天工会话后端」的运行归因（本轮由 Subagent 投递触发时，
//!   sidecar 把对应 Run 置完成并经 Hook 回报激活会话）。

mod bindings;
mod sidecar_client;
mod specs;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use serde_json::Value;
use tiangong_plugin_subagent_protocol::{
    MENTION_CANDIDATES, PLUGIN_ID, PLUGIN_VERSION, SESSION_TURN_FINISHED, TOOL_OPERATIONS,
};

mod descriptor {
    pub const NAME: &str = "Subagent";
}

fn plugin_err(message: impl Into<String>) -> PluginError {
    PluginError::Message(message.into())
}

/// 工具级失败的 ToolResult（区别于 PluginError：后者会被宿主映射为
/// 「未注册的工具」，丢失真实原因）。
fn tool_failure(summary: impl Into<String>) -> ToolResult {
    ToolResult {
        ok: false,
        summary: summary.into(),
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 1,
        execution: None,
    }
}

struct Component;

impl Guest for Component {
    fn describe() -> Result<PluginDescriptor, PluginError> {
        Ok(PluginDescriptor {
            id: PLUGIN_ID.to_string(),
            name: descriptor::NAME.to_string(),
            version: PLUGIN_VERSION.to_string(),
        })
    }

    fn tool_specs() -> Result<Vec<ToolSpec>, PluginError> {
        Ok(specs::TOOL_SPECS
            .iter()
            .map(|(name, description, schema)| ToolSpec {
                name: (*name).to_string(),
                description: (*description).to_string(),
                input_schema: (*schema).to_string(),
            })
            .collect())
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(vec![specs::PROMPT_SECTION.to_string()])
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        // 失败一律映射为 ok:false 的 ToolResult：PluginError 会被宿主吞成
        // 「未注册的工具」（adapter 对 Err 返回 None），模型将收到误导信息。
        if !TOOL_OPERATIONS.contains(&call.name.as_str()) {
            return Ok(tool_failure(format!("未知的 Subagent 工具: {}", call.name)));
        }
        // sidecar 对全部工具操作返回 ToolOutcome 形状，直接映射。
        let response = match sidecar_client::invoke_raw(&call.name, &call.arguments) {
            Ok(response) => response,
            Err(error) => {
                return Ok(tool_failure(format!(
                    "{} 执行失败（sidecar 通道）: {error}",
                    call.name
                )));
            }
        };
        let outcome: Value = match serde_json::from_str(&response) {
            Ok(outcome) => outcome,
            Err(error) => {
                return Ok(tool_failure(format!(
                    "解析 {} 响应失败: {error}",
                    call.name
                )));
            }
        };
        Ok(ToolResult {
            ok: outcome.get("ok").and_then(Value::as_bool).unwrap_or(false),
            summary: outcome
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stdout: outcome
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stderr: String::new(),
            exit_code: outcome.get("exit_code").and_then(Value::as_i64).unwrap_or(
                if outcome.get("ok").and_then(Value::as_bool) == Some(true) {
                    0
                } else {
                    1
                },
            ) as i32,
            execution: None,
        })
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(_workspace: Option<String>, _full_trust: bool) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_session_ready(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_started(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_finished(session_json: String, turn_start_idx: u32) -> Result<(), PluginError> {
        forward_turn_finished(&session_json, turn_start_idx);
        Ok(())
    }

    fn on_session_ended(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }
}

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_contribution_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("本插件无设置页视图"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("本插件无视图资源"))
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        // mention 候选经此通道下发（loader 以专用方法名探测）。
        if request.method == "__tiangong.mention_candidates.v1" {
            // UI 辅助能力：sidecar 不可用时降级为空列表，不阻塞输入补全。
            let payload =
                sidecar_client::invoke_raw(MENTION_CANDIDATES, "{}").unwrap_or_else(|error| {
                    eprintln!("[subagent] 提及候选获取失败，降级为空: {error}");
                    "[]".to_string()
                });
            return Ok(ViewMessageResponse { payload });
        }
        Err(plugin_err("本插件无视图消息通道"))
    }
}

/// 提取本轮信息并转发 sidecar（天工会话后端的完成回报）。
fn forward_turn_finished(session_json: &str, turn_start_idx: u32) {
    let Ok(session) = serde_json::from_str::<Value>(session_json) else {
        return;
    };
    let Some(session_id) = session.get("id").and_then(Value::as_str) else {
        return;
    };
    let messages = session
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let anchor_id = session.get("turn_start_message_id").and_then(Value::as_str);
    // 本轮用户锚点：优先按 id 定位，回退剔除 Notice 前移后的索引。
    let anchor = anchor_id
        .and_then(|id| {
            messages
                .iter()
                .find(|message| message.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| messages.get(turn_start_idx as usize));
    let (user_text, turn_status, anchor_idx) = match anchor {
        Some(anchor) => {
            let idx = messages
                .iter()
                .position(|message| std::ptr::eq(message, anchor))
                .unwrap_or(0);
            (
                message_text(anchor),
                anchor
                    .get("turn_status")
                    .and_then(Value::as_str)
                    .unwrap_or("success")
                    .to_string(),
                idx,
            )
        }
        None => (String::new(), "success".to_string(), 0),
    };
    // 最终回复：本轮（锚点之后）倒序第一条 assistant（优先 summary 阶段，
    // 过滤空文本）。切片从锚点之后开始——锚点已排除，倒序第一条即本轮
    // 最后一条消息（最终答案），不得再跳过。
    let mut assistant_text = String::new();
    for phase in ["summary", "normal"] {
        for message in messages[(anchor_idx + 1).min(messages.len())..]
            .iter()
            .rev()
        {
            if message.get("role").and_then(Value::as_str) == Some("assistant")
                && message
                    .get("phase")
                    .and_then(Value::as_str)
                    .unwrap_or("normal")
                    == phase
            {
                let text = message_text(message);
                if !text.trim().is_empty() {
                    assistant_text = text;
                    break;
                }
            }
        }
        if !assistant_text.is_empty() {
            break;
        }
    }
    let payload = serde_json::json!({
        "session_id": session_id,
        "user_text": user_text,
        "assistant_text": assistant_text,
        "turn_status": turn_status,
    });
    if let Ok(body) = serde_json::to_string(&payload)
        && let Err(error) = sidecar_client::invoke_raw(SESSION_TURN_FINISHED, &body)
    {
        // 通知型钩子：失败不阻断会话，sidecar 侧有孤儿运行恢复兜底。
        eprintln!("[subagent] 转发 turn 完成失败: {error}");
    }
}

fn message_text(message: &Value) -> String {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

bindings::export!(Component with_types_in bindings);
