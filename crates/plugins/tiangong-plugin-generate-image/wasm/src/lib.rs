//! Generate-Image 插件的 WASM 桥接组件。

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
use tiangong_plugin_generate_image_protocol::{
    Generate, GenerateRequest, GenerateResponse, TOOL_GENERATE_IMAGE,
};

mod descriptor {
    pub const ID: &str = tiangong_plugin_generate_image_protocol::PLUGIN_ID;
    pub const NAME: &str = "Generate-Image";
    pub const VERSION: &str = tiangong_plugin_generate_image_protocol::PLUGIN_VERSION;
}

fn plugin_err(message: impl Into<String>) -> PluginError {
    PluginError::Message(message.into())
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
            name: TOOL_GENERATE_IMAGE.to_string(),
            description: "根据文字描述生成图片。每次调用会等待生成完成后返回图片路径。\
            注意：同一轮次中不要重复调用相同 prompt 的 generate_image，\
            拿到图片结果后应直接继续后续任务（如编写 HTML、组合排版等）。"
                .to_string(),
            input_schema: r#"{"type":"object","properties":{"prompt":{"type":"string","description":"图片描述，建议使用英文以获得更好效果"},"width":{"type":"integer","description":"宽度（可选）"},"height":{"type":"integer","description":"高度（可选）"},"style":{"type":"string","description":"风格（可选）"}},"required":["prompt"]}"#
                .to_string(),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_GENERATE_IMAGE => handle_generate(&call),
            other => Err(plugin_err(format!("未知的 Image 工具: {other}"))),
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

    fn on_session_ready(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_started(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_finished(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_session_ended(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }
}

fn handle_generate(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if prompt.is_empty() {
        return Err(plugin_err("缺少必填参数 prompt"));
    }

    let width = args.get("width").and_then(Value::as_u64).map(|v| v as u32);
    let height = args.get("height").and_then(Value::as_u64).map(|v| v as u32);
    let style = args.get("style").and_then(Value::as_str).map(String::from);

    let request = GenerateRequest {
        prompt,
        width,
        height,
        style,
    };
    let response: GenerateResponse = sidecar_client::invoke::<Generate>(&request)
        .map_err(|e| plugin_err(format!("图片生成失败: {e}")))?;

    let markdown = response
        .images
        .iter()
        .enumerate()
        .map(|(i, img)| format!("![图片 {}]({})", i + 1, img.reference))
        .collect::<Vec<_>>()
        .join("\n");
    let summary = format!("图片生成成功（模型：{}）", response.model);

    Ok(ToolResult {
        ok: true,
        summary,
        stdout: markdown,
        stderr: String::new(),
        exit_code: 0,
        execution: None,
    })
}

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_contribution_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("Image 插件无设置页"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("Image 插件无外部资源"))
    }

    fn handle_view_message(
        _request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        Err(plugin_err("Image 插件无设置页消息"))
    }
}
bindings::export!(Component with_types_in bindings);
