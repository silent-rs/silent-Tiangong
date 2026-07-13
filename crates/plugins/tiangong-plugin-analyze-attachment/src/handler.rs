//! 附件分析工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，提供 `analyze_attachment` 工具。
//! 入口层通过 [`crate::should_register`] 按 multimodal 能力条件注册；工具规格仅当
//! `register` 阶段成功缓存 multimodal client 时返回，handler 内部保留 client 缺失兜底。
//!
//! 参数直接从 LLM 传入的命名参数 JSON（`call.arguments`）按 key 取参：附件定位、
//! 媒体收集与请求构造行为与原 `core::execute_attachment_analysis_tool` 保持一致。
//!
//! multimodal 子调用的 token 用量经 [`PluginFeedbackTx::report_token_usage`] 反馈给
//! core，由 core 统一累加到本轮 `accumulated_usage` 并发送 `StreamEvent::TokenUsage`
//! （`ToolOverrideHandler::handle` 返回值不携带 usage，故走语义反馈通道）。

use std::future::Future;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::pin::Pin;
use std::time::Instant;

use serde_json::json;
use tiangong_core::model::{ModelRequest, ToolCall, ToolSpec};
use tiangong_core::session::{Message, MessageRole, Session};
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};
use tiangong_types::{ContentBlock, MediaKind, StoredAsset};

use crate::plugin::{AnalyzeAttachmentPlugin, TOOL_ANALYZE_ATTACHMENT};

impl AnalyzeAttachmentPlugin {
    /// 主分发入口：同步解析参数并返回 owned Future（借用不逃逸到 async 上下文）。
    fn dispatch(
        &self,
        call: &ToolCall,
        session: &Session,
    ) -> Option<Pin<Box<dyn Future<Output = ToolResult> + Send>>> {
        // client 缺失时的防御在 handle_analyze_attachment 内部完成（返回明确错误），
        // 此处不再单独检查，避免误注册时落到 runtime 的“未注册工具”。
        match call.name.as_str() {
            TOOL_ANALYZE_ATTACHMENT => Some(self.handle_analyze_attachment(call, session)),
            _ => None,
        }
    }

    /// 同步解析参数并构造异步执行体。
    ///
    /// 所有对 `call` / `session` 的借用在此函数内完成，move 进 async 块的均为 owned 值。
    fn handle_analyze_attachment(
        &self,
        call: &ToolCall,
        session: &Session,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        let started = Instant::now();
        let tool_name = TOOL_ANALYZE_ATTACHMENT.to_string();

        // app 层已保证满足条件才构造插件，client 必然就绪。
        let client = self.client();

        let instruction = call
            .arguments
            .get("instruction")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("请解析附件内容，并提取与用户问题有关的信息。")
            .to_string();
        let message_id = call
            .arguments
            .get("message_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(String::from);
        let attachment_index = call
            .arguments
            .get("attachment_index")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize);

        // 定位包含附件的用户消息（借用 session，不逃逸）。
        let Some(source) = find_attachment_source_message(session, message_id.as_deref()) else {
            return Box::pin(async move {
                attachment_error(
                    &tool_name,
                    "未找到可解析的附件",
                    "no user message with attachments found",
                    started,
                )
            });
        };

        // 按序号筛选或收集全部媒体资源。
        let assets = match attachment_index {
            Some(index) => {
                let all = collect_message_assets(source);
                match all.get(index) {
                    Some(asset) => vec![asset.clone()],
                    None => {
                        return Box::pin(async move {
                            attachment_error(
                                &tool_name,
                                "附件序号不存在",
                                &format!("attachment_index {index} out of range"),
                                started,
                            )
                        });
                    }
                }
            }
            None => collect_message_assets(source),
        }
        .into_iter()
        .filter(|asset| asset.kind == MediaKind::Image)
        .collect::<Vec<_>>();

        if assets.is_empty() {
            return Box::pin(async move {
                attachment_error(
                    &tool_name,
                    "未找到可解析的图片附件",
                    "selected resources contain no image attachment",
                    started,
                )
            });
        }

        // 构造附件解析请求上下文（owned）。
        let source_text = source.text_content();
        let session_title = session.title.clone();
        let mut attachment_context = vec![
            Message::new(
                MessageRole::User,
                "你是附件解析助手。只根据随消息提供的附件内容和解析要求回答，输出可供主模型直接使用的简洁中文结果。".to_string(),
            ),
            Message::new(
                MessageRole::Assistant,
                "好的，我将作为附件解析助手，根据附件内容和解析要求进行分析。".to_string(),
            ),
        ];
        let mut user_message = Message::new(
            MessageRole::User,
            format!(
                "用户原始消息：{}\n\n解析要求：{}",
                source_text.trim(),
                instruction
            ),
        );
        for asset in assets {
            user_message
                .content
                .push(ContentBlock::Image { asset, data: None });
        }
        attachment_context.push(user_message);

        let req = ModelRequest {
            session_title: format!("{session_title} · attachment-analysis"),
            user_input: String::new(),
            context: attachment_context,
            thinking: None,
            reasoning_effort: None,
            thinking_disabled: false,
        };

        let feedback_tx = self.feedback_tx();

        Box::pin(async move {
            let result = client.complete_async(&req).await;
            let duration_ms = started.elapsed().as_millis() as u64;

            match result {
                Ok(response) => {
                    // 经语义反馈通道把 multimodal 子调用的 token 用量上报给 core，
                    // 由 turn-scoped usage sink 即时累加到本轮 accumulated_usage 并发送
                    // StreamEvent::TokenUsage，确保成本统计与 Done.usage 都包含该消耗
                    // （上下文压缩由主对话 LLM 的用量驱动，与子调用用量无关）。
                    if let Some(tx) = feedback_tx {
                        tx.report_token_usage(response.usage.clone(), TOOL_ANALYZE_ATTACHMENT);
                    }
                    attachment_success(&response.text, &tool_name, duration_ms)
                }
                Err(err) => attachment_failure(&tool_name, "附件解析失败", &err, duration_ms),
            }
        })
    }
}

impl ToolSpecProvider for AnalyzeAttachmentPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        // app 层已保证满足条件才构造插件，无条件暴露工具规格。
        vec![ToolSpec {
            name: TOOL_ANALYZE_ATTACHMENT.to_string(),
            description: "按需调用多模态模型解析用户上传的图片附件。只有当用户问题确实需要查看图片内容时才调用；文档和其他文件应使用对应文件工具。重要：message_id 必须使用用户消息中提示文字所标注的 ID，不要使用其他消息的 ID。".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "instruction": {
                        "type": "string",
                        "description": "希望多模态模型如何解析附件，例如提取文字、描述画面、识别表格、回答与附件有关的问题"
                    },
                    "message_id": {
                        "type": "string",
                        "description": "包含附件的用户消息 ID。省略时使用最近一条包含附件的用户消息"
                    },
                    "attachment_index": {
                        "type": "integer",
                        "description": "附件序号，从 0 开始。省略时解析该消息中的全部附件"
                    }
                },
                "required": ["instruction"]
            }),
        }]
    }
}

impl ToolOverrideHandler for AnalyzeAttachmentPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        _actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
        match self.dispatch(call, session) {
            Some(future) => Box::pin(async move { Some(future.await) }),
            None => Box::pin(async { None }),
        }
    }
}

// ── 消息检索辅助 ──────────────────────────────────────────

/// 判断消息是否携带媒体附件。
fn has_media(msg: &Message) -> bool {
    !collect_message_assets(msg).is_empty()
}

/// 定位用于附件解析的源用户消息。
///
/// 优先按 `message_id` 精确匹配；省略时取最近一条带附件的用户消息。
fn find_attachment_source_message<'a>(
    session: &'a Session,
    message_id: Option<&str>,
) -> Option<&'a Message> {
    if let Some(message_id) = message_id {
        return session
            .messages
            .iter()
            .find(|message| message.id == message_id && has_media(message));
    }
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User && has_media(message))
}

/// 收集消息中的全部稳定资源，保持 content block 原始顺序。
///
/// 新合同直接复用宿主提供的 [`StoredAsset`]；旧 `Media` 块只在此兼容边界转换，
/// 不把旧格式重新泄漏给 Core 或 Provider。
fn collect_message_assets(message: &Message) -> Vec<StoredAsset> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image { asset, .. } | ContentBlock::AssetReference { asset } => {
                Some(asset.clone())
            }
            ContentBlock::Media {
                kind,
                url,
                mime_type,
                title,
            } => Some(stored_asset_from_legacy_media(
                *kind,
                url,
                mime_type.as_deref(),
                title.as_deref(),
            )),
            ContentBlock::Text { .. } | ContentBlock::ModelInstruction { .. } => None,
        })
        .collect()
}

fn stored_asset_from_legacy_media(
    kind: MediaKind,
    url: &str,
    mime_type: Option<&str>,
    title: Option<&str>,
) -> StoredAsset {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let original_name = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| legacy_resource_name(url));
    let mime_type = mime_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| inferred_mime_type(kind, url));
    let size = if url.trim_start().starts_with("data:")
        || url.trim_start().starts_with("http://")
        || url.trim_start().starts_with("https://")
    {
        0
    } else {
        std::fs::metadata(url)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    };

    StoredAsset {
        asset_id: format!("legacy-{:016x}", hasher.finish()),
        local_path: url.to_string(),
        original_name,
        mime_type,
        size,
        kind,
    }
}

fn legacy_resource_name(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.starts_with("data:") {
        return "legacy-attachment".to_string();
    }

    let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    without_query
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "legacy-attachment".to_string())
}

fn inferred_mime_type(kind: MediaKind, url: &str) -> String {
    let trimmed = url.trim();
    if let Some(data_mime) = trimmed
        .strip_prefix("data:")
        .and_then(|value| value.split_once(';').map(|(mime, _)| mime))
        .filter(|mime| mime.contains('/'))
    {
        return data_mime.to_string();
    }

    let path = trimmed
        .split(['?', '#'])
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    match kind {
        MediaKind::Image if path.ends_with(".jpg") || path.ends_with(".jpeg") => "image/jpeg",
        MediaKind::Image if path.ends_with(".webp") => "image/webp",
        MediaKind::Image if path.ends_with(".gif") => "image/gif",
        MediaKind::Image => "image/png",
        MediaKind::Video if path.ends_with(".webm") => "video/webm",
        MediaKind::Video => "video/mp4",
        MediaKind::Audio if path.ends_with(".wav") => "audio/wav",
        MediaKind::Audio if path.ends_with(".ogg") => "audio/ogg",
        MediaKind::Audio => "audio/mpeg",
        MediaKind::File => "application/octet-stream",
    }
    .to_string()
}

// ── ToolResult 构造辅助 ──────────────────────────────────────────

/// 成功结果。
fn attachment_success(text: &str, tool_name: &str, duration_ms: u64) -> ToolResult {
    let summary = "附件解析完成".to_string();
    ToolResult {
        ok: true,
        summary: summary.clone(),
        stdout: text.to_string(),
        stderr: String::new(),
        exit_code: 0,
        execution: Some(ToolExecutionRecord {
            tool_name: tool_name.to_string(),
            args: Vec::new(),
            duration_ms,
            ok: true,
            exit_code: 0,
            summary,
        }),
    }
}

/// 调用失败时的统一结果构造（对齐原 runtime 实现）。
fn attachment_failure(
    tool_name: &str,
    prefix: &str,
    err: &anyhow::Error,
    duration_ms: u64,
) -> ToolResult {
    let summary = format!("{prefix}：{err}");
    ToolResult {
        ok: false,
        summary: summary.clone(),
        stdout: String::new(),
        stderr: err.to_string(),
        exit_code: 1,
        execution: Some(ToolExecutionRecord {
            tool_name: tool_name.to_string(),
            args: Vec::new(),
            duration_ms,
            ok: false,
            exit_code: 1,
            summary,
        }),
    }
}

/// 参数/前置条件不满足时的错误结果。
///
/// 与原 runtime 实现一致：所有结果都携带 `ToolExecutionRecord`，便于审计与前端展示。
fn attachment_error(tool_name: &str, summary: &str, stderr: &str, started: Instant) -> ToolResult {
    ToolResult {
        ok: false,
        summary: summary.to_string(),
        stdout: String::new(),
        stderr: stderr.to_string(),
        exit_code: 1,
        execution: Some(ToolExecutionRecord {
            tool_name: tool_name.to_string(),
            args: Vec::new(),
            duration_ms: started.elapsed().as_millis() as u64,
            ok: false,
            exit_code: 1,
            summary: summary.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_asset(asset_id: &str, local_path: &str) -> StoredAsset {
        StoredAsset {
            asset_id: asset_id.to_string(),
            local_path: local_path.to_string(),
            original_name: format!("{asset_id}.png"),
            mime_type: "image/png".to_string(),
            size: 4,
            kind: MediaKind::Image,
        }
    }

    #[test]
    fn collects_new_and_legacy_resources_in_content_order() {
        let first = stored_asset("asset-1", "/tmp/asset-1.png");
        let second = stored_asset("asset-2", "/tmp/asset-2.png");
        let mut message = Message::new(MessageRole::User, "查看附件");
        message.content.extend([
            ContentBlock::Image {
                asset: first.clone(),
                data: Some("CURRENT_BASE64".to_string()),
            },
            ContentBlock::AssetReference {
                asset: second.clone(),
            },
            ContentBlock::Media {
                kind: MediaKind::Image,
                url: "/tmp/legacy.jpg".to_string(),
                mime_type: Some("image/jpeg".to_string()),
                title: Some("legacy.jpg".to_string()),
            },
        ]);

        let assets = collect_message_assets(&message);

        assert_eq!(assets.len(), 3);
        assert_eq!(assets[0], first);
        assert_eq!(assets[1], second);
        assert_eq!(assets[2].local_path, "/tmp/legacy.jpg");
        assert_eq!(assets[2].original_name, "legacy.jpg");
        assert_eq!(assets[2].mime_type, "image/jpeg");
        assert!(assets[2].asset_id.starts_with("legacy-"));
    }

    #[test]
    fn legacy_data_url_does_not_become_a_resource_name() {
        let asset = stored_asset_from_legacy_media(
            MediaKind::Image,
            "data:image/png;base64,AAAA",
            None,
            None,
        );

        assert_eq!(asset.original_name, "legacy-attachment");
        assert_eq!(asset.mime_type, "image/png");
        assert_eq!(asset.size, 0);
    }

    #[test]
    fn legacy_image_mime_is_inferred_from_path() {
        let asset = stored_asset_from_legacy_media(
            MediaKind::Image,
            "https://example.com/photo.jpg?size=large",
            None,
            None,
        );

        assert_eq!(asset.original_name, "photo.jpg");
        assert_eq!(asset.mime_type, "image/jpeg");
        assert_eq!(asset.size, 0);
    }
}
