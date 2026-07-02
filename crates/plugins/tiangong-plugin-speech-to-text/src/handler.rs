//! 语音转文本工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，提供 `speech_to_text` 工具。
//! 参数直接从 LLM 传入的命名参数 JSON（`call.arguments`）按 key 取参，
//! 后端调用复用 [`tiangong_core::media::transcribe_audio`] facade（与原 runtime 一致）。

use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use serde_json::json;
use tiangong_core::media::MediaServiceError;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};

use crate::plugin::SpeechToTextPlugin;

/// 工具名常量。
const TOOL_SPEECH_TO_TEXT: &str = "speech_to_text";

impl SpeechToTextPlugin {
    /// 主分发入口：同步解析参数并返回 owned Future（借用不逃逸到 async 上下文）。
    fn dispatch(
        &self,
        call: &ToolCall,
    ) -> Option<Pin<Box<dyn Future<Output = ToolResult> + Send>>> {
        match call.name.as_str() {
            TOOL_SPEECH_TO_TEXT => Some(self.handle_speech_to_text(call)),
            _ => None,
        }
    }

    /// 同步解析参数，构造异步执行体。
    fn handle_speech_to_text(
        &self,
        call: &ToolCall,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        let file_path = call
            .arguments
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if file_path.is_empty() {
            return Box::pin(async { missing_arg("file_path 不能为空") });
        }

        let Some(models) = self.models_config() else {
            return Box::pin(async { media_unavailable() });
        };

        // 读取音频文件与 MIME 推断在同步阶段完成（文件 IO 同步即可）。
        let audio_data = match std::fs::read(&file_path) {
            Ok(data) => data,
            Err(e) => {
                let summary = format!("读取音频文件失败：{e}");
                let stderr = e.to_string();
                return Box::pin(async move {
                    ToolResult {
                        ok: false,
                        summary,
                        stdout: String::new(),
                        stderr,
                        exit_code: 1,
                        execution: None,
                    }
                });
            }
        };

        // 根据扩展名推断 MIME 类型
        let mime_type = match std::path::Path::new(&file_path)
            .extension()
            .and_then(|e| e.to_str())
        {
            Some("mp3") => "audio/mpeg",
            Some("wav") => "audio/wav",
            Some("ogg") | Some("oga") => "audio/ogg",
            Some("flac") => "audio/flac",
            Some("webm") => "audio/webm",
            Some("m4a") => "audio/mp4",
            _ => "audio/mpeg",
        }
        .to_string();

        let language = call
            .arguments
            .get("language")
            .and_then(|v| v.as_str())
            .map(String::from);

        Box::pin(async move {
            let started = Instant::now();
            let tool_name = TOOL_SPEECH_TO_TEXT.to_string();
            let result =
                tiangong_core::media::transcribe_audio(&models, audio_data, mime_type, language)
                    .await;
            let duration_ms = started.elapsed().as_millis() as u64;

            match result {
                Ok(output) => {
                    let lang_info = output
                        .response
                        .language
                        .as_deref()
                        .map(|l| format!("，语言：{l}"))
                        .unwrap_or_default();
                    let dur_info = output
                        .response
                        .duration
                        .map(|d| format!("，音频时长：{:.1}s", d))
                        .unwrap_or_default();
                    let summary = format!(
                        "语音识别成功（模型：{}{}{dur_info}）",
                        output.resolved.model, lang_info
                    );
                    ToolResult {
                        ok: true,
                        summary: summary.clone(),
                        stdout: output.response.text,
                        stderr: String::new(),
                        exit_code: 0,
                        execution: Some(ToolExecutionRecord {
                            tool_name,
                            args: vec![],
                            duration_ms,
                            ok: true,
                            exit_code: 0,
                            summary,
                        }),
                    }
                }
                Err(err) => media_failure(&tool_name, "语音识别", &err, duration_ms),
            }
        })
    }
}

impl ToolSpecProvider for SpeechToTextPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        if !self.has_stt() {
            return Vec::new();
        }
        vec![ToolSpec {
            name: TOOL_SPEECH_TO_TEXT.to_string(),
            description: "将音频文件转录为文本".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "音频文件路径" },
                    "language": { "type": "string", "description": "语言提示（可选）" }
                },
                "required": ["file_path"]
            }),
        }]
    }
}

impl ToolOverrideHandler for SpeechToTextPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        _session: &tiangong_core::session::Session,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
        match self.dispatch(call) {
            Some(future) => Box::pin(async move { Some(future.await) }),
            None => Box::pin(async { None }),
        }
    }
}

// ── ToolResult 构造辅助 ──────────────────────────────────────────

/// 缺少必填参数时的错误结果。
fn missing_arg(message: &str) -> ToolResult {
    ToolResult {
        ok: false,
        summary: message.to_string(),
        stdout: String::new(),
        stderr: message.to_string(),
        exit_code: 1,
        execution: None,
    }
}

/// 媒体能力未就绪（配置未注入）时的错误结果。
fn media_unavailable() -> ToolResult {
    ToolResult {
        ok: false,
        summary: "媒体能力未初始化".to_string(),
        stdout: String::new(),
        stderr: "media plugin not registered".to_string(),
        exit_code: 1,
        execution: None,
    }
}

/// 媒体调用失败时的统一结果构造（对齐原 runtime::RuntimeEngine::media_error_summary）。
fn media_failure(
    tool_name: &str,
    prefix: &str,
    err: &MediaServiceError,
    duration_ms: u64,
) -> ToolResult {
    let summary = if err.is_timeout() || err.is_config() {
        err.to_string()
    } else {
        format!("{prefix}失败：{err}")
    };
    ToolResult {
        ok: false,
        summary: summary.clone(),
        stdout: String::new(),
        stderr: err.to_string(),
        exit_code: 1,
        execution: Some(ToolExecutionRecord {
            tool_name: tool_name.to_string(),
            args: vec![],
            duration_ms,
            ok: false,
            exit_code: 1,
            summary,
        }),
    }
}
