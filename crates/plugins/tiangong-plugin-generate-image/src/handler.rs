//! 图片生成工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，提供 `generate_image` 工具。
//! 参数直接从 LLM 传入的命名参数 JSON（`call.arguments`）按 key 取参，
//! 后端调用复用 [`tiangong_core::media::generate_image`] facade（与原 runtime 实现一致）。

use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use serde_json::json;
use tiangong_core::media::MediaServiceError;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};

use crate::plugin::GenerateImagePlugin;

/// 工具名常量。
const TOOL_GENERATE_IMAGE: &str = "generate_image";

impl GenerateImagePlugin {
    /// 主分发入口：同步解析参数并返回 owned Future（借用不逃逸到 async 上下文）。
    fn dispatch(
        &self,
        call: &ToolCall,
    ) -> Option<Pin<Box<dyn Future<Output = ToolResult> + Send>>> {
        match call.name.as_str() {
            TOOL_GENERATE_IMAGE => Some(self.handle_generate_image(call)),
            _ => None,
        }
    }

    /// 同步解析参数，构造异步执行体。
    fn handle_generate_image(
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

        let Some(models) = self.models_config() else {
            return Box::pin(async { media_unavailable() });
        };

        let width = call
            .arguments
            .get("width")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let height = call
            .arguments
            .get("height")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let style = call
            .arguments
            .get("style")
            .and_then(|v| v.as_str())
            .map(String::from);

        Box::pin(async move {
            let started = Instant::now();
            let tool_name = TOOL_GENERATE_IMAGE.to_string();
            let result =
                tiangong_core::media::generate_image(&models, prompt, width, height, style).await;
            let duration_ms = started.elapsed().as_millis() as u64;

            match result {
                Ok(output) => {
                    let mut parts = Vec::new();
                    for (i, img) in output.response.images.iter().enumerate() {
                        if let Some(url) = &img.url {
                            parts.push(format!("![图片 {}]({})", i + 1, url));
                        } else if let Some(b64) = &img.b64_data {
                            parts.push(format!("![图片 {}](data:image/png;base64,{})", i + 1, b64));
                        }
                    }
                    let markdown = parts.join("\n");
                    let summary = format!("图片生成成功（模型：{}）", output.resolved.model);
                    ToolResult {
                        ok: true,
                        summary: summary.clone(),
                        stdout: markdown,
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
                Err(err) => media_failure(&tool_name, "图片生成", &err, duration_ms),
            }
        })
    }
}

impl ToolSpecProvider for GenerateImagePlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        if !self.has_image() {
            return Vec::new();
        }
        vec![ToolSpec {
            name: TOOL_GENERATE_IMAGE.to_string(),
            description: "根据文字描述生成图片。每次调用会等待生成完成后返回图片路径。\
            注意：同一轮次中不要重复调用相同 prompt 的 generate_image，\
            拿到图片结果后应直接继续后续任务（如编写 HTML、组合排版等）。"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "图片描述，建议使用英文以获得更好效果" },
                    "width": { "type": "integer", "description": "宽度（可选）" },
                    "height": { "type": "integer", "description": "高度（可选）" },
                    "style": { "type": "string", "description": "风格（可选）" }
                },
                "required": ["prompt"]
            }),
        }]
    }
}

impl ToolOverrideHandler for GenerateImagePlugin {
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
