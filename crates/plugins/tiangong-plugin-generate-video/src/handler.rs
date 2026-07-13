//! 视频生成工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，提供 `generate_video` 工具。
//! 参数直接从 LLM 传入的命名参数 JSON（`call.arguments`）按 key 取参，
//! 后端调用复用 [`tiangong_core::media::generate_video`] facade（与原 runtime 实现一致）。

use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use serde_json::json;
use tiangong_core::media::MediaServiceError;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};

use crate::plugin::GenerateVideoPlugin;

/// 工具名常量。
const TOOL_GENERATE_VIDEO: &str = "generate_video";

impl GenerateVideoPlugin {
    /// 主分发入口：同步解析参数并返回 owned Future（借用不逃逸到 async 上下文）。
    fn dispatch(
        &self,
        call: &ToolCall,
    ) -> Option<Pin<Box<dyn Future<Output = ToolResult> + Send>>> {
        match call.name.as_str() {
            TOOL_GENERATE_VIDEO => Some(self.handle_generate_video(call)),
            _ => None,
        }
    }

    /// 同步解析参数，构造异步执行体。
    fn handle_generate_video(
        &self,
        call: &ToolCall,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        let prompt = call
            .arguments
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if prompt.is_empty() {
            return Box::pin(async { missing_arg("prompt 不能为空") });
        }

        let endpoint = self.endpoint();

        let duration = match parse_optional_u32_arg(call, "duration") {
            Ok(v) => v,
            Err(msg) => {
                return Box::pin(async move { invalid_arg(&msg) });
            }
        };
        let resolution = call
            .arguments
            .get("resolution")
            .and_then(|v| v.as_str())
            .map(String::from);

        Box::pin(async move {
            let started = Instant::now();
            let tool_name = TOOL_GENERATE_VIDEO.to_string();
            let resolved = endpoint.to_resolved();
            let result =
                tiangong_core::media::generate_video_with(&resolved, prompt, duration, resolution)
                    .await;
            let duration_ms = started.elapsed().as_millis() as u64;

            match result {
                Ok(output) => {
                    use tiangong_media::video::VideoGenStatus;

                    let (ok, summary, stdout, stderr, exit_code) = match output.response.status {
                        VideoGenStatus::Completed {
                            video_url,
                            duration,
                        } => {
                            let duration_line = duration
                                .map(|seconds| format!("\nDuration: {seconds:.1}s"))
                                .unwrap_or_default();
                            (
                                true,
                                format!("视频生成成功（模型：{}）", output.resolved.model),
                                format!("Video URL: {video_url}{duration_line}"),
                                String::new(),
                                0,
                            )
                        }
                        VideoGenStatus::Pending => (
                            true,
                            format!("视频生成任务已提交（模型：{}）", output.resolved.model),
                            format!("Task ID: {}\nStatus: pending", output.response.task_id),
                            String::new(),
                            0,
                        ),
                        VideoGenStatus::Processing { progress } => {
                            let progress_line = progress
                                .map(|p| format!("\nProgress: {p:.1}%"))
                                .unwrap_or_default();
                            (
                                true,
                                format!("视频生成任务处理中（模型：{}）", output.resolved.model),
                                format!(
                                    "Task ID: {}\nStatus: processing{progress_line}",
                                    output.response.task_id
                                ),
                                String::new(),
                                0,
                            )
                        }
                        VideoGenStatus::Failed { error } => (
                            false,
                            format!("视频生成失败：{error}"),
                            String::new(),
                            error,
                            1,
                        ),
                    };
                    ToolResult {
                        ok,
                        summary: summary.clone(),
                        stdout,
                        stderr,
                        exit_code,
                        execution: Some(ToolExecutionRecord {
                            tool_name,
                            args: vec![],
                            duration_ms,
                            ok,
                            exit_code,
                            summary,
                        }),
                    }
                }
                Err(err) => media_failure(&tool_name, "视频生成", &err, duration_ms),
            }
        })
    }
}

impl ToolSpecProvider for GenerateVideoPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: TOOL_GENERATE_VIDEO.to_string(),
            description: "根据文字描述生成视频，成功时返回结构化视频资源".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "视频描述" },
                    "duration": { "type": "integer", "description": "视频时长，单位秒（可选）" },
                    "resolution": { "type": "string", "description": "分辨率，如 720p、1080p（可选）" }
                },
                "required": ["prompt"]
            }),
        }]
    }
}

impl ToolOverrideHandler for GenerateVideoPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        _session: &mut tiangong_core::session::Session,
        _actor_id: &str,
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

/// 解析可选的 u32 参数，避免 `u64 as u32` 静默截断。
fn parse_optional_u32_arg(call: &ToolCall, name: &str) -> Result<Option<u32>, String> {
    match call.arguments.get(name).and_then(|v| v.as_u64()) {
        None => Ok(None),
        Some(v) => u32::try_from(v)
            .map(Some)
            .map_err(|_| format!("参数 {name} 超出 u32 范围（{v}）")),
    }
}

/// 无效参数时的错误结果。
fn invalid_arg(message: &str) -> ToolResult {
    ToolResult {
        ok: false,
        summary: message.to_string(),
        stdout: String::new(),
        stderr: message.to_string(),
        exit_code: 1,
        execution: None,
    }
}
