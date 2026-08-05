//! Analyze-Attachment 插件的 WASM 桥接组件。
//!
//! 缓存 session JSON（生命周期钩子），handle_tool 时从消息提取图片路径，
//! 转发到 sidecar 做多模态分析。

mod bindings;
mod sidecar_client;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use serde_json::Value;
use tiangong_plugin_analyze_attachment_protocol::{
    Analyze, AnalyzeRequest, AnalyzeResponse, TOOL_ANALYZE_ATTACHMENT,
};

mod descriptor {
    pub const ID: &str = tiangong_plugin_analyze_attachment_protocol::PLUGIN_ID;
    pub const NAME: &str = "Analyze-Attachment";
    pub const VERSION: &str = tiangong_plugin_analyze_attachment_protocol::PLUGIN_VERSION;
}

fn plugin_err(message: impl Into<String>) -> PluginError {
    PluginError::Message(message.into())
}

// 缓存的会话消息（thread-local，生命周期钩子注入）。
thread_local! {
    static SESSION_MESSAGES: std::cell::RefCell<Vec<Value>> = const { std::cell::RefCell::new(Vec::new()) };
}

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
            name: TOOL_ANALYZE_ATTACHMENT.to_string(),
            description: "按需调用多模态模型解析用户上传的图片附件。只有当用户问题确实需要查看图片内容时才调用；文档和其他文件应使用对应文件工具。重要：message_id 必须使用用户消息中提示文字所标注的 ID，不要使用其他消息的 ID。".to_string(),
            input_schema: r#"{"type":"object","properties":{"instruction":{"type":"string","description":"希望多模态模型如何解析附件，例如提取文字、描述画面、识别表格、回答与附件有关的问题"},"message_id":{"type":"string","description":"包含附件的用户消息 ID。省略时使用最近一条包含附件的用户消息"},"attachment_index":{"type":"integer","description":"附件序号，从 0 开始。省略时解析该消息中的全部附件"}},"required":["instruction"]}"#
                .to_string(),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(vec![format!(
            "## 附件分析工具\n\
             当用户消息明确列出需要分析的图片资源，且回答确实需要查看图片内容时，可调用 `{TOOL_ANALYZE_ATTACHMENT}`。\n\
             调用时必须使用该用户消息标注的 `message_id`；`attachment_index` 从 0 开始，对应消息中的资源顺序。\n\
             文档和其他文件应使用对应文件工具；普通文本对话、无需查看图片内容或消息未提供可分析图片时，不要调用此工具。"
        )])
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_ANALYZE_ATTACHMENT => handle_analyze(&call),
            other => Err(plugin_err(format!("未知的 Attachment 工具: {other}"))),
        }
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

    fn on_session_ready(session_json: String) -> Result<(), PluginError> {
        cache_session(&session_json);
        Ok(())
    }

    fn on_turn_started(session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        cache_session(&session_json);
        Ok(())
    }

    fn on_turn_finished(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_session_ended(_session_json: String) -> Result<(), PluginError> {
        SESSION_MESSAGES.with(|m| m.borrow_mut().clear());
        Ok(())
    }
}

/// 缓存 session JSON 中的消息列表。
fn cache_session(session_json: &str) {
    let session: Value = serde_json::from_str(session_json).unwrap_or(Value::Null);
    let messages = session
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    SESSION_MESSAGES.with(|m| {
        *m.borrow_mut() = messages;
    });
}

fn handle_analyze(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    let instruction = args
        .get("instruction")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let message_id = args
        .get("message_id")
        .and_then(Value::as_str)
        .map(String::from);
    let attachment_index = args
        .get("attachment_index")
        .and_then(Value::as_u64)
        .map(|v| v as usize);

    // 从缓存的 session 消息中定位包含附件的用户消息。
    let (user_text, images) = SESSION_MESSAGES.with(|m| {
        let messages = m.borrow();
        find_attachment_source(&messages, message_id.as_deref(), attachment_index)
    });

    if images.is_empty() {
        return Ok(ToolResult {
            ok: false,
            summary: "未找到可解析的图片附件".to_string(),
            stdout: String::new(),
            stderr: "no image attachment found".to_string(),
            exit_code: 1,
            execution: None,
        });
    }

    let request = AnalyzeRequest {
        instruction,
        user_message_text: user_text,
        images,
    };
    let response: AnalyzeResponse = sidecar_client::invoke::<Analyze>(&request)
        .map_err(|e| plugin_err(format!("附件分析失败: {e}")))?;

    Ok(ToolResult {
        ok: true,
        summary: "附件解析完成".to_string(),
        stdout: response.text,
        stderr: String::new(),
        exit_code: 0,
        execution: None,
    })
}

/// 从缓存消息中定位附件源消息，提取图片路径。
///
/// 返回 (用户消息文本, 图片本地路径列表)。
fn find_attachment_source(
    messages: &[Value],
    message_id: Option<&str>,
    attachment_index: Option<usize>,
) -> (String, Vec<String>) {
    // 定位消息：优先按 message_id，否则取最后一条带附件的用户消息。
    let source = if let Some(id) = message_id {
        messages
            .iter()
            .find(|msg| msg.get("id").and_then(Value::as_str) == Some(id))
    } else {
        messages
            .iter()
            .rev()
            .find(|msg| msg.get("role").and_then(Value::as_str) == Some("user"))
    };

    let Some(source) = source else {
        return (String::new(), Vec::new());
    };

    // 提取文本内容。
    let user_text = source
        .get("text_content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // 从 content blocks 提取图片路径。
    let content = source.get("content").and_then(Value::as_array);
    let mut all_images = Vec::new();
    if let Some(content) = content {
        for block in content {
            // ContentBlock::Image { asset: { local_path } }
            if let Some(asset) = block
                .get("asset")
                .or_else(|| block.get("AssetReference").and_then(|a| a.get("asset")))
                && let Some(path) = asset.get("local_path").and_then(Value::as_str)
                && !path.is_empty()
            {
                all_images.push(path.to_string());
            }
        }
    }

    // 按序号筛选或返回全部。
    let images = match attachment_index {
        Some(index) => all_images
            .get(index)
            .cloned()
            .map(|p| vec![p])
            .unwrap_or_default(),
        None => all_images,
    };

    (user_text, images)
}

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_contribution_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("Attachment 插件无设置页"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("Attachment 插件无外部资源"))
    }

    fn handle_view_message(
        _request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        Err(plugin_err("Attachment 插件无设置页消息"))
    }
}

bindings::export!(Component with_types_in bindings);
