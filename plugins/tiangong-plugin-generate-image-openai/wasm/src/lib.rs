//! Generate-Image-OpenAI 插件的 WASM 桥接组件。
//!
//! 只做参数解析、sidecar 转发与配置 UI 桥接。生图、配置持久化全部在 sidecar 进程内。

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
use tiangong_plugin_generate_image_openai_protocol::{
    Empty, Generate, GenerateRequest, GenerateResponse, GetConfig, ImageOperation, Reconfigure,
    SetConfig, TOOL_GENERATE_IMAGE,
};

mod descriptor {
    pub const ID: &str = tiangong_plugin_generate_image_openai_protocol::PLUGIN_ID;
    pub const NAME: &str = "Generate-Image-OpenAI";
    pub const VERSION: &str = tiangong_plugin_generate_image_openai_protocol::PLUGIN_VERSION;
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
            description: "根据文字描述生成图片（通过 OpenAI 兼容的 Chat Completions 接口）。\
            每次调用等待生成完成后返回图片路径。\
            注意：同一轮次中不要重复调用相同 prompt 的 generate_image，\
            拿到图片结果后应直接继续后续任务（如编写 HTML、组合排版等）。"
                .to_string(),
            input_schema: r#"{"type":"object","properties":{"prompt":{"type":"string","description":"图片描述，建议使用英文以获得更好效果"}},"required":["prompt"]}"#
                .to_string(),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_GENERATE_IMAGE => handle_generate(&call),
            other => Err(plugin_err(format!("未知的工具: {other}"))),
        }
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(_workspace: Option<String>, _full_trust: bool) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
        // Core 配置事件只作为重新读取配置的触发器。
        let _ = sidecar_client::invoke::<Reconfigure>(&Empty {});
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

    let request = GenerateRequest { prompt };
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

// ── UI 能力（设置页）──

const SETTINGS_HTML: &str = include_str!("settings.html");
const SETTINGS_CSS: &str = include_str!("settings.css");
const SETTINGS_JS: &str = include_str!("settings.js");

fn settings_html() -> String {
    SETTINGS_HTML
        .replace("/*__SETTINGS_CSS__*/", SETTINGS_CSS)
        .replace("/*__SETTINGS_JS__*/", SETTINGS_JS)
}

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(vec![Contribution {
            id: "generate-image-openai-settings".to_string(),
            title: "OpenAI 生图".to_string(),
            description: "通过 Chat Completions 接口生成图片的配置".to_string(),
            icon: "image".to_string(),
            group: "plugins".to_string(),
            has_view: true,
        }])
    }

    fn open_view(contribution_id: String) -> Result<ViewResponse, PluginError> {
        if contribution_id != "generate-image-openai-settings" {
            return Err(plugin_err(format!(
                "未知的 contribution: {contribution_id}"
            )));
        }
        Ok(ViewResponse {
            html: settings_html(),
        })
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("无外部资源"))
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        let payload = match request.method.as_str() {
            "bootstrap" => invoke_for_ui::<GetConfig>(&Empty {})?,
            "save_config" => {
                let selection = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析配置失败: {e}")))?;
                invoke_for_ui::<SetConfig>(&selection)?
            }
            other => return Err(plugin_err(format!("未知的设置页消息: {other}"))),
        };
        Ok(ViewMessageResponse { payload })
    }
}

fn invoke_for_ui<O>(request: &O::Request) -> Result<String, PluginError>
where
    O: ImageOperation,
    O::Response: serde::Serialize,
{
    let response = sidecar_client::invoke::<O>(request).map_err(|e| plugin_err(e.to_string()))?;
    serde_json::to_string(&response)
        .map_err(|e| plugin_err(format!("序列化 {} 响应失败: {e}", O::NAME)))
}

bindings::export!(Component with_types_in bindings);
