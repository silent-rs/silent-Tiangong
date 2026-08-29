//! Speech-To-Text 插件的 WASM 桥接组件。

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
use tiangong_plugin_speech_to_text_protocol::{
    Empty, RecordCancel, RecordControlRequest, RecordStart, RecordStartRequest,
    RecordStartResponse, RecordStop, RecordStopResponse, TOOL_SPEECH_TO_TEXT, Transcribe,
    TranscribeRequest, TranscribeResponse,
};

mod descriptor {
    pub const ID: &str = tiangong_plugin_speech_to_text_protocol::PLUGIN_ID;
    pub const NAME: &str = "Speech-To-Text";
    pub const VERSION: &str = tiangong_plugin_speech_to_text_protocol::PLUGIN_VERSION;
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
            name: TOOL_SPEECH_TO_TEXT.to_string(),
            description: "将音频文件转录为文本".to_string(),
            input_schema: r#"{"type":"object","properties":{"file_path":{"type":"string","description":"音频文件路径（仅允许 ~/.tiangong/media 目录下的音频文件）"},"language":{"type":"string","description":"语言提示（可选）"}},"required":["file_path"]}"#
                .to_string(),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_SPEECH_TO_TEXT => handle_transcribe(&call),
            other => Err(plugin_err(format!("未知的 STT 工具: {other}"))),
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

fn handle_transcribe(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    let file_path = args
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if file_path.is_empty() {
        return Err(plugin_err("缺少必填参数 file_path"));
    }

    let language = args
        .get("language")
        .and_then(Value::as_str)
        .map(String::from);

    let request = TranscribeRequest {
        file_path,
        language,
    };
    let response: TranscribeResponse = sidecar_client::invoke::<Transcribe>(&request)
        .map_err(|e| plugin_err(format!("语音识别失败: {e}")))?;

    let lang_info = response
        .language
        .as_deref()
        .map(|l| format!("，语言：{l}"))
        .unwrap_or_default();
    let dur_info = response
        .duration
        .map(|d| format!("，音频时长：{:.1}s", d))
        .unwrap_or_default();
    let summary = format!(
        "语音识别成功（模型：{}{}{dur_info}）",
        response.model, lang_info
    );

    Ok(ToolResult {
        ok: true,
        summary,
        stdout: response.text,
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
        Err(plugin_err("STT 插件无设置页"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("STT 插件无外部资源"))
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        let payload = match request.method.as_str() {
            "transcribe" => {
                let req: TranscribeRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析 transcribe 请求失败: {e}")))?;
                let response: TranscribeResponse = sidecar_client::invoke::<Transcribe>(&req)
                    .map_err(|e| plugin_err(format!("语音识别失败: {e}")))?;
                serde_json::to_string(&response)
                    .map_err(|e| plugin_err(format!("序列化 transcribe 响应失败: {e}")))?
            }
            "record_start" => {
                let req: RecordStartRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析 record_start 请求失败: {e}")))?;
                let response: RecordStartResponse = sidecar_client::invoke::<RecordStart>(&req)
                    .map_err(|e| plugin_err(format!("开始录音失败: {e}")))?;
                serde_json::to_string(&response)
                    .map_err(|e| plugin_err(format!("序列化 record_start 响应失败: {e}")))?
            }
            "record_stop" => {
                let req: RecordControlRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析 record_stop 请求失败: {e}")))?;
                let response: RecordStopResponse = sidecar_client::invoke::<RecordStop>(&req)
                    .map_err(|e| plugin_err(format!("停止录音失败: {e}")))?;
                serde_json::to_string(&response)
                    .map_err(|e| plugin_err(format!("序列化 record_stop 响应失败: {e}")))?
            }
            "record_cancel" => {
                let req: RecordControlRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析 record_cancel 请求失败: {e}")))?;
                let _: Empty = sidecar_client::invoke::<RecordCancel>(&req)
                    .map_err(|e| plugin_err(format!("取消录音失败: {e}")))?;
                "{}".to_string()
            }
            other => return Err(plugin_err(format!("未知的 STT 消息: {other}"))),
        };
        Ok(ViewMessageResponse { payload })
    }
}
bindings::export!(Component with_types_in bindings);
