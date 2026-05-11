//! Memory Recall 工具注入、执行与结果构造
//!
//! 提供 recall_memory / analyze_attachment 两类工具的注入、执行和结果序列化。

use crate::memory::turn_result::compact_single_memory_text;
use crate::model::ToolSpec;
use crate::react::message::latest_user_message;
use crate::session::{Message, MessageRole, Session, now_text};
use std::sync::mpsc::Sender as StdSender;
use tiangong_types::{MemoryRecallHitSummary, StreamEvent};

pub(crate) type MemoryRecallToolOutput =
    (crate::tool::ToolResult, tiangong_types::TokenUsage, bool);

const RUNTIME_ROUGH_RECALL_MAX_HITS: usize = 5;
const RUNTIME_RECALL_CONTEXT_MAX_ITEMS: usize = 24;
const RUNTIME_RECALL_MIN_SCORE: f64 = 0.3;

// ── recall_memory 工具注入 ──

pub(crate) fn inject_memory_recall_tool(tools: &mut Vec<ToolSpec>) {
    if tools.iter().any(|tool| tool.name == "recall_memory") {
        return;
    }

    tools.push(ToolSpec {
        name: "recall_memory".to_string(),
        description: "按需回忆历史上下文、跨会话结果、之前的工具输出或生成产物。用户提到刚刚、刚才、上次、之前、那个、继续、这张图、生成的图片等历史指代时，应先调用此工具。".to_string(),
        input_schema: serde_json::json!({
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
    });
}

// ── recall_memory 工具执行 ──

pub(crate) async fn execute_memory_recall_tool(
    call: &crate::model::ToolCall,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
    session: &Session,
) -> MemoryRecallToolOutput {
    let started = std::time::Instant::now();
    let Some(handle) = memory_handle else {
        return (
            memory_recall_tool_result(
                false,
                "记忆系统未启用",
                String::new(),
                "memory disabled".to_string(),
                1,
                Vec::new(),
                started,
            ),
            tiangong_types::TokenUsage::default(),
            false,
        );
    };

    let query = call
        .arguments
        .get("query")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| latest_user_message(session));
    if query.is_empty() {
        return (
            memory_recall_tool_result(
                false,
                "缺少回忆查询",
                String::new(),
                "recall_memory.query is empty".to_string(),
                1,
                Vec::new(),
                started,
            ),
            tiangong_types::TokenUsage::default(),
            false,
        );
    }

    let reason = call
        .arguments
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
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

    let response = handle
        .recall_context(tiangong_memory::MemoryRecallRequest {
            query: query.to_string(),
            reason: (!reason.is_empty()).then(|| reason.to_string()),
            expected,
            context: build_memory_recall_context(session),
            limit,
        })
        .await;

    let memory_usage = tiangong_types::TokenUsage::from(response.usage.clone());

    if response.hits.is_empty() {
        return (
            memory_recall_tool_result(
                true,
                "未找到相关记忆",
                if response.content.trim().is_empty() {
                    format!("未找到与「{query}」相关的历史记忆。")
                } else {
                    response.content
                },
                String::new(),
                0,
                vec![query.to_string()],
                started,
            ),
            memory_usage,
            false,
        );
    }

    let stdout = if response.content.trim().is_empty() {
        "没有发现当前上下文之外的增量记忆。".to_string()
    } else {
        response.content
    };
    let allow_memory_context = !stdout
        .trim()
        .starts_with("没有发现当前上下文之外的增量记忆");

    (
        memory_recall_tool_result(
            true,
            format!("命中 {} 条相关记忆并完成整理", response.hits.len()),
            stdout,
            String::new(),
            0,
            vec![query.to_string()],
            started,
        ),
        memory_usage,
        allow_memory_context,
    )
}

fn memory_recall_tool_result(
    ok: bool,
    summary: impl Into<String>,
    stdout: String,
    stderr: String,
    exit_code: i32,
    args: Vec<String>,
    started: std::time::Instant,
) -> crate::tool::ToolResult {
    let summary = summary.into();
    crate::tool::ToolResult {
        ok,
        summary: summary.clone(),
        stdout,
        stderr,
        exit_code,
        execution: Some(crate::tool::ToolExecutionRecord {
            tool_name: "recall_memory".to_string(),
            args,
            duration_ms: started.elapsed().as_millis() as u64,
            ok,
            exit_code,
            summary,
        }),
    }
}

// ── analyze_attachment 工具执行 ──

// ── 重复调用拦截 ──

pub(crate) fn duplicate_memory_recall_tool_result() -> MemoryRecallToolOutput {
    let started = std::time::Instant::now();
    (
        memory_recall_tool_result(
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

// ── 上下文构建辅助 ──

fn build_memory_recall_context(session: &Session) -> Vec<String> {
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
            let content = compact_single_memory_text(&message.content, 900);
            (!content.is_empty()).then(|| format!("{role}: {content}"))
        })
        .take(30)
        .collect::<Vec<_>>();
    items.reverse();
    items
}

pub(crate) async fn maybe_inject_runtime_memory_recall(
    session: &mut Session,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
    query: &str,
    trigger: &str,
    reason: &str,
    next_action: Option<&str>,
    stream_tx: Option<&StdSender<StreamEvent>>,
) -> bool {
    let Some(handle) = memory_handle else {
        return false;
    };
    let query = query.trim();
    if query.is_empty() {
        return false;
    }

    // 立即通知前端开始检索，避免用户看到空白等待
    send_stream_event(
        stream_tx,
        StreamEvent::MemoryRecallStart {
            strategy: "runtime".to_string(),
        },
    );

    let context = tiangong_memory::RuntimeRecallContext {
        query: query.to_string(),
        reason: (!reason.trim().is_empty()).then(|| reason.trim().to_string()),
        trigger: (!trigger.trim().is_empty()).then(|| trigger.trim().to_string()),
        next_action: next_action
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(String::from),
        current_context: build_runtime_context(session),
        policy: tiangong_memory::RuntimeRecallPolicy::default(),
    };

    send_stream_event(
        stream_tx,
        StreamEvent::MemoryRecallProgress {
            phase: "粗回忆".to_string(),
        },
    );
    let rough_hits = handle.rough_recall(context.clone()).await;
    let filtered_rough: Vec<_> = rough_hits
        .into_iter()
        .filter(|h| h.score >= RUNTIME_RECALL_MIN_SCORE)
        .collect();

    send_stream_event(
        stream_tx,
        StreamEvent::MemoryRecallProgress {
            phase: "评估充分性".to_string(),
        },
    );
    let sufficiency = handle
        .evaluate_recall_sufficiency(context.clone(), filtered_rough.clone())
        .await;

    if sufficiency.should_upgrade_to_hybrid {
        send_stream_event(
            stream_tx,
            StreamEvent::MemoryRecallProgress {
                phase: "深度回忆".to_string(),
            },
        );
        let response = handle
            .recall_context(tiangong_memory::MemoryRecallRequest {
                query: sufficiency
                    .next_query
                    .clone()
                    .unwrap_or_else(|| query.to_string()),
                reason: Some(format!("runtime:{trigger}; {reason}")),
                expected: runtime_expected_items(trigger, next_action),
                context: context.current_context.clone(),
                limit: context.policy.deep_limit,
            })
            .await;

        let filtered_deep: Vec<_> = response
            .hits
            .iter()
            .filter(|h| h.score >= RUNTIME_RECALL_MIN_SCORE)
            .cloned()
            .collect();

        if filtered_deep.is_empty() {
            send_no_result_event(stream_tx);
            inject_no_result_message(session, query);
            return false;
        }

        let hit_count = filtered_deep.len();
        let hits = filtered_deep
            .iter()
            .map(|h| MemoryRecallHitSummary {
                title: h.title.clone(),
                summary: compact_single_memory_text(&h.summary, 120),
                score: h.score,
            })
            .collect::<Vec<_>>();
        send_stream_event(stream_tx, StreamEvent::MemoryRecallDone { hit_count, hits });
        inject_runtime_recall_response(session, &filtered_deep);
        return true;
    }

    if filtered_rough.is_empty() {
        send_no_result_event(stream_tx);
        inject_no_result_message(session, query);
        return false;
    }

    let result = inject_runtime_rough_hits(session, &filtered_rough);
    let hit_summaries = filtered_rough
        .iter()
        .take(RUNTIME_ROUGH_RECALL_MAX_HITS)
        .map(|h| MemoryRecallHitSummary {
            title: h.title.clone(),
            summary: compact_single_memory_text(&h.summary, 120),
            score: h.score,
        })
        .collect::<Vec<_>>();
    send_stream_event(
        stream_tx,
        StreamEvent::MemoryRecallDone {
            hit_count: filtered_rough.len(),
            hits: hit_summaries,
        },
    );
    result
}

fn build_runtime_context(session: &Session) -> Vec<String> {
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
            let content = compact_single_memory_text(&message.content, 700);
            (!content.is_empty()).then(|| format!("{role}: {content}"))
        })
        .take(RUNTIME_RECALL_CONTEXT_MAX_ITEMS)
        .collect::<Vec<_>>();
    items.reverse();
    items
}

fn runtime_expected_items(trigger: &str, next_action: Option<&str>) -> Vec<String> {
    let text = format!("{}\n{}", trigger, next_action.unwrap_or_default()).to_ascii_lowercase();
    let mut expected = vec!["decision".to_string(), "tool_result".to_string()];
    if text.contains("file")
        || text.contains("path")
        || text.contains("文件")
        || text.contains(".rs")
    {
        expected.push("file".to_string());
        expected.push("code_context".to_string());
    }
    if text.contains("command") || text.contains("run_command") || text.contains("命令") {
        expected.push("command_usage".to_string());
    }
    expected
}

fn inject_runtime_recall_response(session: &mut Session, hits: &[tiangong_memory::RecallHit]) {
    let hit_count = hits.len();
    let hit_lines = hits
        .iter()
        .map(|h| {
            format!(
                "- [{:.2}] {}: {}",
                h.score,
                compact_single_memory_text(&h.title, 120),
                compact_single_memory_text(&h.summary, 260)
            )
        })
        .collect::<Vec<_>>();
    let mut msg = format!("[记忆检索] 策略: deep\n命中 {hit_count} 条\n");
    if !hit_lines.is_empty() {
        msg.push_str(&hit_lines.join("\n"));
    }
    append_runtime_recall_message(session, msg);
}

fn inject_runtime_rough_hits(session: &mut Session, hits: &[tiangong_memory::RecallHit]) -> bool {
    let lines = hits
        .iter()
        .take(RUNTIME_ROUGH_RECALL_MAX_HITS)
        .map(|hit| {
            format!(
                "- [{:.2}] {}: {}",
                hit.score,
                compact_single_memory_text(&hit.title, 120),
                compact_single_memory_text(&hit.summary, 260)
            )
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return false;
    }
    let count = lines.len();
    append_runtime_recall_message(
        session,
        format!(
            "[记忆检索] 策略: rough\n命中 {count} 条\n{}",
            lines.join("\n")
        ),
    );
    true
}

fn append_runtime_recall_message(session: &mut Session, content: String) {
    let mut message = Message {
        id: scru128::new().to_string(),
        role: MessageRole::Tool,
        content,
        reasoning_content: String::new(),
        reasoning_signature: None,
        worker_id: None,
        media: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: Some("runtime_memory_recall".to_string()),
        tool_result_is_error: false,
        compact: true,
        created_at: now_text(),
    };
    message.content = compact_single_memory_text(&message.content, 1800);
    session.messages.push(message);
    session.updated_at = now_text();
}

fn send_no_result_event(stream_tx: Option<&StdSender<StreamEvent>>) {
    send_stream_event(
        stream_tx,
        StreamEvent::MemoryRecallDone {
            hit_count: 0,
            hits: Vec::new(),
        },
    );
}

fn inject_no_result_message(session: &mut Session, query: &str) {
    append_runtime_recall_message(
        session,
        format!("[记忆检索] 未检索到与「{query}」相关的有效记忆"),
    );
}

fn send_stream_event(stream_tx: Option<&StdSender<StreamEvent>>, event: StreamEvent) {
    if let Some(tx) = stream_tx {
        let _ = tx.send(event);
    }
}
