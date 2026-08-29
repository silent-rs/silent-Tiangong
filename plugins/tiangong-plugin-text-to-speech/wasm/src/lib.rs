//! Text-To-Speech 插件的 WASM 桥接组件。
//!
//! 只做参数解析与 sidecar 转发。模型配置、供应商调用、音频落盘全部在 sidecar 进程内。

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
use tiangong_plugin_text_to_speech_protocol::{
    Empty, ListModels, ListModelsResponse, ListVoices, ListVoicesResponse, Play, PlayRequest,
    PlayResponse, PlayStatus, PlayStatusResponse, Stop, Synthesize, SynthesizeRequest,
    SynthesizeResponse, TOOL_TEXT_TO_SPEECH,
};

mod descriptor {
    pub const ID: &str = tiangong_plugin_text_to_speech_protocol::PLUGIN_ID;
    pub const NAME: &str = "Text-To-Speech";
    pub const VERSION: &str = tiangong_plugin_text_to_speech_protocol::PLUGIN_VERSION;
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
            name: TOOL_TEXT_TO_SPEECH.to_string(),
            description: "将文本转换为语音音频文件".to_string(),
            input_schema: r#"{"type":"object","properties":{"text":{"type":"string","description":"待合成文本"},"voice":{"type":"string","description":"音色（可选）"},"speed":{"type":"number","description":"语速（可选）"}},"required":["text"]}"#
                .to_string(),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_TEXT_TO_SPEECH => handle_synthesize(&call),
            other => Err(plugin_err(format!("未知的 TTS 工具: {other}"))),
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

fn handle_synthesize(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return Err(plugin_err("缺少必填参数 text"));
    }

    let voice = args.get("voice").and_then(Value::as_str).map(String::from);
    let speed = args.get("speed").and_then(Value::as_f64);

    let request = SynthesizeRequest { text, voice, speed };
    let response: SynthesizeResponse = sidecar_client::invoke::<Synthesize>(&request)
        .map_err(|e| plugin_err(format!("语音合成失败: {e}")))?;

    let duration_info = response
        .duration
        .map(|d| format!("，时长 {:.1}s", d))
        .unwrap_or_default();
    let summary = format!("语音合成成功（模型：{}{}）", response.model, duration_info);

    Ok(ToolResult {
        ok: true,
        summary,
        stdout: format!("音频文件已保存到：{}", response.file_path),
        stderr: String::new(),
        exit_code: 0,
        execution: None,
    })
}

// ── UI 能力（TTS 无设置页，但提供前端可经 bridge.call 调用的业务方法）──

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_contribution_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("TTS 插件无设置页"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("TTS 插件无外部资源"))
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        let payload = match request.method.as_str() {
            "synthesize" => {
                let req: SynthesizeRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析 synthesize 请求失败: {e}")))?;
                let response: SynthesizeResponse = sidecar_client::invoke::<Synthesize>(&req)
                    .map_err(|e| plugin_err(format!("语音合成失败: {e}")))?;
                serde_json::to_string(&response)
                    .map_err(|e| plugin_err(format!("序列化 synthesize 响应失败: {e}")))?
            }
            "play" => {
                let req: PlayRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析 play 请求失败: {e}")))?;
                let response: PlayResponse = sidecar_client::invoke::<Play>(&req)
                    .map_err(|e| plugin_err(format!("播放失败: {e}")))?;
                serde_json::to_string(&response)
                    .map_err(|e| plugin_err(format!("序列化 play 响应失败: {e}")))?
            }
            "stop" => {
                let _: Empty = serde_json::from_str(&request.payload).unwrap_or_default();
                let _: Empty = sidecar_client::invoke::<Stop>(&Empty {})
                    .map_err(|e| plugin_err(format!("停止播放失败: {e}")))?;
                "{}".to_string()
            }
            "play_status" => {
                let _: Empty = serde_json::from_str(&request.payload).unwrap_or_default();
                let response: PlayStatusResponse = sidecar_client::invoke::<PlayStatus>(&Empty {})
                    .map_err(|e| plugin_err(format!("查询播放状态失败: {e}")))?;
                serde_json::to_string(&response)
                    .map_err(|e| plugin_err(format!("序列化 play_status 响应失败: {e}")))?
            }
            "list_models" => {
                let _: Empty = serde_json::from_str(&request.payload).unwrap_or_default();
                let response: ListModelsResponse = sidecar_client::invoke::<ListModels>(&Empty {})
                    .map_err(|e| plugin_err(format!("列出模型失败: {e}")))?;
                serde_json::to_string(&response)
                    .map_err(|e| plugin_err(format!("序列化 list_models 响应失败: {e}")))?
            }
            "list_voices" => {
                let _: Empty = serde_json::from_str(&request.payload).unwrap_or_default();
                let response: ListVoicesResponse = sidecar_client::invoke::<ListVoices>(&Empty {})
                    .map_err(|e| plugin_err(format!("列出音色失败: {e}")))?;
                serde_json::to_string(&response)
                    .map_err(|e| plugin_err(format!("序列化 list_voices 响应失败: {e}")))?
            }
            other => return Err(plugin_err(format!("未知的 TTS 消息: {other}"))),
        };
        Ok(ViewMessageResponse { payload })
    }
}
bindings::export!(Component with_types_in bindings);
