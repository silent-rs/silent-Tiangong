//! recall_memory 工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，提供 `recall_memory` 工具。
//! 核心检索逻辑迁自原 `tiangong-core/src/memory/recall.rs`，改为通过插件字段
//! （`memory_handle` / `feedback_tx`）和 `handle` 入参 `&Session` 获取运行时上下文。
//!
//! 与原 core 实现的差异：
//! - 流事件（`MemoryRecallStart` / `Progress` / `Done`）改经 `PluginFeedbackTx` 转发，
//!   不再直接持有 `stream_tx`。`Done` 事件在 async 块内（recall 完成后）发送，
//!   通过提前 clone `feedback_tx` move 进 async 块实现。
//! - 「本轮已回忆」去重由 [`MemoryPlugin::mark_recall_attempted`] 承载，
//!   [`crate::plugin::MemoryPlugin`] 的 `on_turn_started` 每轮重置。
//! - 会话消息读取（构建上下文 / 回退 query）从 `handle` 入参 `&Session` 获取。
//! - 原输出的 `allow_memory_context` 标志不再跨 handler 边界传递：recall 结果的
//!   引导文案直接写入 `ToolResult.summary`，engine 层无需再特判 recall_memory 文案。

use std::time::Instant;

use serde_json::json;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::{MessageRole, Session};
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};
use tiangong_memory::MemoryRecallRequest;
use tiangong_types::{MemoryRecallHitSummary, StreamEvent};

use crate::plugin::MemoryPlugin;

const TOOL_RECALL_MEMORY: &str = "recall_memory";

impl ToolSpecProvider for MemoryPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: TOOL_RECALL_MEMORY.to_string(),
            description: "按需回忆历史上下文、跨会话结果、之前的工具输出或生成产物。用户提到刚刚、刚才、上次、之前、那个、继续、这张图、生成的图片等历史指代时，应先调用此工具。".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "要回忆的内容，结合用户当前请求改写成可检索查询"
                    },
                    "reason": {
                        "type": "string",
                        "description": "为什么需要回忆，简述当前任务依赖的历史语境"
                    },
                    "expected": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "期望找回的内容类型，如 media、file、tool_result、decision、code_context"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多返回多少条记忆，默认 5，最大 10"
                    }
                },
                "required": ["query"]
            }),
        }]
    }
}

impl ToolOverrideHandler for MemoryPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        _actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        // 本轮已回忆 → 去重分支（补发 Start/Done 让状态栏有过渡）。
        if self.mark_recall_attempted(&session.id) {
            self.emit_skip_events();
            let (result, _usage, _allow) = duplicate_recall_result();
            return Box::pin(async move { Some(result) });
        }

        // 同步预解析参数（借用 &self/&call/&session），捕获 owned 数据后在 async 块内
        // 完成 async 的 recall_context 调用，避免借用逃逸。
        let started = Instant::now();
        let Some(handle) = self.memory_handle() else {
            // 记忆系统未启用：补发 Start/Done，返回降级结果。
            self.emit_skip_events();
            let result = recall_tool_result(
                false,
                "记忆系统未启用",
                String::new(),
                "memory disabled".to_string(),
                1,
                Vec::new(),
                started,
            );
            return Box::pin(async move { Some(result) });
        };

        // query 解析：优先取参数，为空时回退到最近一条用户消息。
        let fallback_query = latest_user_message(session);
        let query = call
            .arguments
            .get("query")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .unwrap_or(fallback_query);

        if query.is_empty() {
            self.emit_skip_events();
            let result = recall_tool_result(
                false,
                "缺少回忆查询",
                String::new(),
                "recall_memory.query is empty".to_string(),
                1,
                Vec::new(),
                started,
            );
            return Box::pin(async move { Some(result) });
        }

        let reason = call
            .arguments
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let expected = call
            .arguments
            .get("expected")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let limit = call
            .arguments
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(5)
            .clamp(1, 10) as usize;

        self.emit_memory_event(StreamEvent::MemoryRecallStart {
            strategy: "auto".to_string(),
        });

        // 进度回调与 Done 事件共用同一份 feedback_tx clone（各自 move 进闭包）。
        let progress_tx = self.feedback_tx();
        let done_tx = self.feedback_tx();
        let context = build_recall_context(session);
        let query_for_result = query.clone();

        Box::pin(async move {
            let response = handle
                .recall_context(MemoryRecallRequest {
                    query,
                    reason: (!reason.is_empty()).then_some(reason),
                    expected,
                    context,
                    limit,
                    progress: Some(std::sync::Arc::new(move |phase: &str| {
                        if let Some(tx) = progress_tx.as_ref() {
                            tx.send_stream_event(StreamEvent::MemoryRecallProgress {
                                phase: phase.to_string(),
                            });
                        }
                    })),
                })
                .await;

            // 命中摘要（用于 Done 事件 + 结果组装）。
            let hits_summary: Vec<MemoryRecallHitSummary> = response
                .hits
                .iter()
                .map(|hit| MemoryRecallHitSummary {
                    title: hit.title.clone(),
                    summary: hit.summary.clone(),
                    score: hit.score,
                })
                .collect();
            let hit_count = hits_summary.len();

            // 发送 Done 事件（recall 完成后，带命中数与摘要）。
            if let Some(tx) = done_tx.as_ref() {
                tx.send_stream_event(StreamEvent::MemoryRecallDone {
                    hit_count,
                    hits: hits_summary,
                });
            }

            Some(assemble_recall_result(response, query_for_result, started))
        })
    }
}

impl MemoryPlugin {
    /// 通过 feedback_tx 发送流事件（通道未注入或已关闭时静默丢弃）。
    fn emit_memory_event(&self, event: StreamEvent) {
        if let Some(tx) = self.feedback_tx() {
            tx.send_stream_event(event);
        }
    }

    /// 发送 skip 策略的 Start/Done 事件（去重分支 / 降级分支用）。
    fn emit_skip_events(&self) {
        self.emit_memory_event(StreamEvent::MemoryRecallStart {
            strategy: "skip".to_string(),
        });
        self.emit_memory_event(StreamEvent::MemoryRecallDone {
            hit_count: 0,
            hits: Vec::new(),
        });
    }
}

// ── 结果组装（迁自原 recall.rs，改为独立函数）──

/// 把 MemoryRecallResponse 组装成 ToolResult。
///
/// 结果文案内嵌引导语（原由 engine 层 `build_memory_recall_feedback` 拼接），
/// 使 `allow_memory_context` 不再需要跨 handler 边界传递。
fn assemble_recall_result(
    response: tiangong_memory::MemoryRecallResponse,
    query: String,
    started: Instant,
) -> ToolResult {
    if response.hits.is_empty() {
        let stdout = if response.content.trim().is_empty() {
            format!("未找到与「{query}」相关的历史记忆。")
        } else {
            response.content
        };
        let header = "recall_memory 已完成，但没有可用的增量历史记忆。请基于当前上下文继续完成用户原始目标；不要再次调用 recall_memory。";
        return recall_tool_result(
            true,
            "未找到相关记忆",
            format!("{header}\n\n{stdout}"),
            String::new(),
            0,
            vec![query],
            started,
        );
    }

    let stdout = if response.content.trim().is_empty() {
        "没有发现当前上下文之外的增量记忆。".to_string()
    } else {
        response.content
    };
    // 原 allow_memory_context 语义：stdout 非「没有增量记忆」时，引导语鼓励直接使用结果。
    let allow_memory_context = !stdout
        .trim()
        .starts_with("没有发现当前上下文之外的增量记忆");
    let header = if allow_memory_context {
        "recall_memory 已完成。以下是可直接使用的回忆结果，请基于这些内容继续完成用户原始目标；不要再次调用 recall_memory，除非用户提出新的历史查询。"
    } else {
        "recall_memory 已完成，结果如下。请基于当前上下文继续完成用户原始目标；不要再次调用 recall_memory。"
    };

    recall_tool_result(
        true,
        format!("命中 {} 条相关记忆并完成整理", response.hits.len()),
        format!("{header}\n\n{stdout}"),
        String::new(),
        0,
        vec![query],
        started,
    )
}

/// 构造 recall_memory 的 ToolResult。
fn recall_tool_result(
    ok: bool,
    summary: impl Into<String>,
    stdout: String,
    stderr: String,
    exit_code: i32,
    args: Vec<String>,
    started: Instant,
) -> ToolResult {
    let summary = summary.into();
    ToolResult {
        ok,
        summary: summary.clone(),
        stdout,
        stderr,
        exit_code,
        execution: Some(ToolExecutionRecord {
            tool_name: TOOL_RECALL_MEMORY.to_string(),
            args,
            duration_ms: started.elapsed().as_millis() as u64,
            ok,
            exit_code,
            summary,
        }),
    }
}

/// 本轮已回忆的去重结果（迁自原 duplicate_memory_recall_tool_result）。
fn duplicate_recall_result() -> (ToolResult, tiangong_types::TokenUsage, bool) {
    let started = Instant::now();
    (
        recall_tool_result(
            true,
            "本轮已完成回忆，跳过重复调用",
            "recall_memory 本轮已经执行过，回忆结果已经注入当前上下文。请直接基于已有回忆结果完成用户原始目标，不要再次调用 recall_memory。"
                .to_string(),
            String::new(),
            0,
            vec!["duplicate-recall".to_string()],
            started,
        ),
        tiangong_types::TokenUsage::default(),
        false,
    )
}

/// 取最近一条用户消息（query 参数为空时回退用）。
fn latest_user_message(session: &Session) -> String {
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.text_content())
        .unwrap_or_default()
}

/// 构建检索上下文：取最近 30 条消息，按 "role: content" 格式化。
fn build_recall_context(session: &Session) -> Vec<String> {
    let mut items = session
        .messages
        .iter()
        .rev()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => return None,
                MessageRole::Tool => message.tool_name.as_deref().unwrap_or("tool"),
            };
            let content = compact_memory_text(&message.text_content(), 900);
            (!content.is_empty()).then(|| format!("{role}: {content}"))
        })
        .take(30)
        .collect::<Vec<_>>();
    items.reverse();
    items
}

/// 压缩单条记忆文本：去空行 + 截断到 max_chars。
fn compact_memory_text(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut clipped = normalized.chars().take(max_chars).collect::<String>();
    clipped.push_str("...");
    clipped
}
