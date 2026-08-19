//! Screenshot Input 插件的 WASM 逻辑层。

mod bindings;
mod sidecar_client;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use tiangong_plugin_screenshot_input_protocol::{Capture, CaptureRequest, CaptureResponse};

struct Component;

fn plugin_err(message: impl Into<String>) -> PluginError {
    PluginError::Message(message.into())
}

impl Guest for Component {
    fn describe() -> Result<PluginDescriptor, PluginError> {
        Ok(PluginDescriptor {
            id: tiangong_plugin_screenshot_input_protocol::PLUGIN_ID.to_string(),
            name: "Screenshot Input".to_string(),
            version: tiangong_plugin_screenshot_input_protocol::PLUGIN_VERSION.to_string(),
        })
    }

    fn tool_specs() -> Result<Vec<ToolSpec>, PluginError> {
        Ok(Vec::new())
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        Err(plugin_err(format!("截图输入插件不提供工具: {}", call.name)))
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

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_contribution_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("截图输入界面由 plugin.json 声明"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("截图输入插件无 WASM 页面资源"))
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        if request.method != "capture" {
            return Err(plugin_err(format!("未知的截图消息: {}", request.method)));
        }
        let response: CaptureResponse = sidecar_client::invoke::<Capture>(&CaptureRequest {})
            .map_err(|error| plugin_err(format!("区域截图失败: {error}")))?;
        let payload = serde_json::to_string(&response)
            .map_err(|error| plugin_err(format!("序列化截图响应失败: {error}")))?;
        Ok(ViewMessageResponse { payload })
    }
}

bindings::export!(Component with_types_in bindings);
