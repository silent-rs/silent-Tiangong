//! Generate-Video 插件的 WASM 桥接组件。

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
use tiangong_plugin_generate_video_protocol::{
    Generate, GenerateRequest, GenerateResponse, TOOL_GENERATE_VIDEO,
};

mod descriptor {
    pub const ID: &str = tiangong_plugin_generate_video_protocol::PLUGIN_ID;
    pub const NAME: &str = "Generate-Video";
    pub const VERSION: &str = tiangong_plugin_generate_video_protocol::PLUGIN_VERSION;
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
            name: TOOL_GENERATE_VIDEO.to_string(),
            description: "根据文字描述生成视频，成功时返回结构化视频资源".to_string(),
            input_schema: r#"{"type":"object","properties":{"prompt":{"type":"string","description":"视频描述"},"duration":{"type":"integer","description":"视频时长，单位秒（可选）"},"resolution":{"type":"string","description":"分辨率，如 720p、1080p（可选）"}},"required":["prompt"]}"#
                .to_string(),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_GENERATE_VIDEO => handle_generate(&call),
            other => Err(plugin_err(format!("未知的 Video 工具: {other}"))),
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

    let duration = args
        .get("duration")
        .and_then(Value::as_u64)
        .map(|v| v as u32);
    let resolution = args
        .get("resolution")
        .and_then(Value::as_str)
        .map(String::from);

    let request = GenerateRequest {
        prompt,
        duration,
        resolution,
    };
    let response: GenerateResponse = sidecar_client::invoke::<Generate>(&request)
        .map_err(|e| plugin_err(format!("视频生成失败: {e}")))?;

    let status = &response.status;
    let (ok, summary, stdout, stderr, exit_code) = if status.completed {
        let url = status.video_url.clone().unwrap_or_default();
        let dur_line = status
            .duration
            .map(|d| format!("\nDuration: {d:.1}s"))
            .unwrap_or_default();
        (
            true,
            format!("视频生成成功（模型：{}）", response.model),
            format!("Video URL: {url}{dur_line}"),
            String::new(),
            0,
        )
    } else if status.pending {
        (
            true,
            format!("视频生成任务已提交（模型：{}）", response.model),
            format!(
                "Task ID: {}\nStatus: pending",
                status.task_id.as_deref().unwrap_or("")
            ),
            String::new(),
            0,
        )
    } else if status.processing {
        let progress_line = status
            .progress
            .map(|p| format!("\nProgress: {p:.1}%"))
            .unwrap_or_default();
        (
            true,
            format!("视频生成任务处理中（模型：{}）", response.model),
            format!(
                "Task ID: {}\nStatus: processing{progress_line}",
                status.task_id.as_deref().unwrap_or("")
            ),
            String::new(),
            0,
        )
    } else {
        let error = status
            .error
            .clone()
            .unwrap_or_else(|| "未知错误".to_string());
        (
            false,
            format!("视频生成失败：{error}"),
            String::new(),
            error,
            1,
        )
    };

    Ok(ToolResult {
        ok,
        summary,
        stdout,
        stderr,
        exit_code,
        execution: None,
    })
}

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_contribution_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("Video 插件无设置页"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("Video 插件无外部资源"))
    }

    fn handle_view_message(
        _request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        Err(plugin_err("Video 插件无设置页消息"))
    }
}
bindings::export!(Component with_types_in bindings);
