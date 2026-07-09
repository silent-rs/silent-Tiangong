//! 文本转语音工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，提供 `text_to_speech` 工具。
//! 参数直接从 LLM 传入的命名参数 JSON（`call.arguments`）按 key 取参，
//! 后端调用复用 [`tiangong_core::media::synthesize_speech`] facade（与原 runtime 一致）。

use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use serde_json::json;
use tiangong_core::media::MediaServiceError;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};

use crate::plugin::TextToSpeechPlugin;

/// 工具名常量。
const TOOL_TEXT_TO_SPEECH: &str = "text_to_speech";

impl TextToSpeechPlugin {
    /// 主分发入口：同步解析参数并返回 owned Future（借用不逃逸到 async 上下文）。
    fn dispatch(
        &self,
        call: &ToolCall,
    ) -> Option<Pin<Box<dyn Future<Output = ToolResult> + Send>>> {
        match call.name.as_str() {
            TOOL_TEXT_TO_SPEECH => Some(self.handle_text_to_speech(call)),
            _ => None,
        }
    }

    /// 同步解析参数，构造异步执行体。
    fn handle_text_to_speech(
        &self,
        call: &ToolCall,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        let text = call
            .arguments
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if text.is_empty() {
            return Box::pin(async { missing_arg("text 不能为空") });
        }

        let endpoint = self.endpoint();

        let voice = call
            .arguments
            .get("voice")
            .and_then(|v| v.as_str())
            .map(String::from);
        let speed = call.arguments.get("speed").and_then(|v| v.as_f64());

        Box::pin(async move {
            let started = Instant::now();
            let tool_name = TOOL_TEXT_TO_SPEECH.to_string();
            let resolved = endpoint.to_resolved();
            let result =
                tiangong_core::media::synthesize_speech_with(&resolved, text, voice, speed, None)
                    .await;
            let duration_ms = started.elapsed().as_millis() as u64;

            match result {
                Ok(output) => {
                    // 扩展名由后端返回的 mime_type 决定，与 GUI command 保持一致。
                    let ext = match output.response.mime_type.as_str() {
                        "audio/mpeg" => "mp3",
                        "audio/wav" => "wav",
                        "audio/opus" => "opus",
                        "audio/aac" => "aac",
                        "audio/flac" => "flac",
                        _ => "mp3",
                    };
                    let file_path = match media_file_path("tts", ext) {
                        Ok(p) => p,
                        Err(e) => {
                            return ToolResult {
                                ok: false,
                                summary: format!("音频文件路径构造失败：{e}"),
                                stdout: String::new(),
                                stderr: e.to_string(),
                                exit_code: 1,
                                execution: None,
                            };
                        }
                    };
                    match std::fs::write(&file_path, &output.response.audio) {
                        Ok(_) => {
                            let duration_info = output
                                .response
                                .duration
                                .map(|d| format!("，时长 {:.1}s", d))
                                .unwrap_or_default();
                            let summary = format!(
                                "语音合成成功（模型：{}{}）",
                                output.resolved.model, duration_info
                            );
                            ToolResult {
                                ok: true,
                                summary: summary.clone(),
                                stdout: format!("音频文件已保存到：{}", file_path.display()),
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
                        Err(e) => ToolResult {
                            ok: false,
                            summary: format!("音频文件写入失败：{e}"),
                            stdout: String::new(),
                            stderr: e.to_string(),
                            exit_code: 1,
                            execution: None,
                        },
                    }
                }
                Err(err) => media_failure(&tool_name, "语音合成", &err, duration_ms),
            }
        })
    }
}

impl ToolSpecProvider for TextToSpeechPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: TOOL_TEXT_TO_SPEECH.to_string(),
            description: "将文本转换为语音音频文件".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "待合成文本" },
                    "voice": { "type": "string", "description": "音色（可选）" },
                    "speed": { "type": "number", "description": "语速（可选）" }
                },
                "required": ["text"]
            }),
        }]
    }
}

impl ToolOverrideHandler for TextToSpeechPlugin {
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

/// 构造 `~/.tiangong/media/<prefix>_<scru128>.<ext>` 媒体文件路径，并确保目录存在。
///
/// 与 GUI command 的媒体存储策略保持一致：固定目录、随机文件名，不暴露自定义路径。
fn media_file_path(prefix: &str, ext: &str) -> std::io::Result<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = std::path::PathBuf::from(home)
        .join(".tiangong")
        .join("media");
    std::fs::create_dir_all(&dir)?;
    let file_name = format!("{}_{}.{}", prefix, scru128::new(), ext);
    Ok(dir.join(file_name))
}

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
#[allow(dead_code)]
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
