use eventsource_stream::Eventsource;
use futures_util::{StreamExt, stream};

use crate::client::DeepSeekClient;
use crate::dsml;
use crate::error::DeepSeekError;
use crate::types::{
    ChatCompletionRequest, ChatCompletionResponse, EventStream, StreamChunk, StreamEvent,
};

pub struct Chat<'c> {
    client: &'c DeepSeekClient,
}

impl<'c> Chat<'c> {
    pub fn new(client: &'c DeepSeekClient) -> Self {
        Self { client }
    }

    pub async fn create(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, DeepSeekError> {
        self.client.post("/chat/completions", &request).await
    }

    /// 创建流式对话。
    ///
    /// SDK 内置文本工具调用协议兜底：当模型把工具调用写进 `content` 文本（原生协议
    /// 或 DSML 协议）而非结构化 `tool_calls` 字段时，SDK 会在正常接收流式响应的同时
    /// 收集协议文本，并在完整响应结束后统一解析为 `StreamEvent::TextProtocolToolCall`。
    pub async fn create_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<EventStream, DeepSeekError> {
        // 仅当请求携带 tools 时才启用文本协议兜底缓冲。即使 tool_choice=none，调用方
        // 也可能保留 tools schema；若模型仍返回文本工具调用，流末会完整识别并交由上层续作。
        let enable_buffer = request.tools.is_some();
        let response = self
            .client
            .post_stream_raw("/chat/completions", &request)
            .await?;

        let sse_stream = response.bytes_stream().eventsource();
        // 第一层：SSE message → Vec<StreamEvent>（单个 chunk 可能产出多个事件）。
        let chunk_stream = stream::unfold(sse_stream, |mut sse_stream| async move {
            match sse_stream.next().await {
                Some(Ok(message)) => {
                    if message.data == "[DONE]" {
                        Some((vec![Ok(StreamEvent::Done)], sse_stream))
                    } else {
                        Some((parse_stream_chunk(&message.data), sse_stream))
                    }
                }
                Some(Err(err)) => Some((
                    vec![Err(DeepSeekError::Stream(err.to_string()))],
                    sse_stream,
                )),
                None => None,
            }
        });
        let flat = chunk_stream.flat_map(stream::iter);

        // 第二层：文本协议兜底缓冲。普通文本正常透传；确认命中文本工具协议后，
        // 等完整响应结束再统一解析，避免按单个 invoke 尾缀提前拆散一组工具调用。
        if !enable_buffer {
            return Ok(Box::pin(flat));
        }

        let state = BufferState::default();
        let buffered = flat
            .scan(state, |state, result| {
                std::future::ready(Some(apply_buffer(state, result)))
            })
            .flat_map(stream::iter);

        Ok(Box::pin(buffered))
    }
}

/// 文本协议缓冲状态机。
///
/// 三态：
/// - `Idle`：正常透传。遇到 `<` 切到 `Probing`，暂存从 `<` 起的文本。
/// - `Probing`：窗口探测。累积 `<` 后的文本（窗口上限 `PROBE_WINDOW_CHARS` 字符），
///   出现已知协议前缀 → `Confirmed`；窗口满仍未出现 → 整块吐出回 `Idle`。
/// - `Confirmed`：确认走文本协议，持续缓冲到完整响应结束后统一解析。
///
/// 探测只匹配两套协议的精确前缀，避免把 `<toolbar>` 等普通内容误判为工具调用。
#[derive(Default)]
struct BufferState {
    /// 是否已出现过结构化 ToolCallStart（出现则永不缓冲）。
    has_structured_tool_call: bool,
    /// 当前状态。
    phase: BufferPhase,
    /// Probing/Confirmed 期间暂存的文本。
    pending: String,
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum BufferPhase {
    #[default]
    Idle,
    /// 遇到 `<`，在窗口内探测协议关键词。
    Probing,
    /// 确认走文本协议，缓冲到流末。
    Confirmed,
}

/// Probing 窗口：`<` 后最多探测多少字符。覆盖两套协议的标记前缀
/// （原生 `<｜tool` 约 5 字符，DSML `<｜｜DSML` 约 6 字符），留足余量。
const PROBE_WINDOW_CHARS: usize = 10;

/// 判断累计文本是否含已知协议前缀（不区分大小写）。
fn contains_protocol_prefix(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("｜tool") || lower.contains("｜｜dsml")
}

/// 对单个事件施加缓冲逻辑，可能产出 0~N 个事件。
fn apply_buffer(
    state: &mut BufferState,
    result: Result<StreamEvent, DeepSeekError>,
) -> Vec<Result<StreamEvent, DeepSeekError>> {
    match result {
        Ok(StreamEvent::TextDelta(delta)) => {
            // 结构化工具调用已出现 → 不可能再走文本协议，直接透传。
            if state.has_structured_tool_call {
                return vec![Ok(StreamEvent::TextDelta(delta))];
            }
            handle_text_delta(state, delta)
                .into_iter()
                .map(Ok)
                .collect()
        }
        Ok(StreamEvent::ToolCallStart { .. }) => {
            state.has_structured_tool_call = true;
            let mut events = flush_pending(state);
            events.push(result);
            events
        }
        Ok(StreamEvent::Usage(usage)) => {
            // Usage 不是内容结束标记。继续等待 Done，确保根据完整 content 统一识别。
            vec![Ok(StreamEvent::Usage(usage))]
        }
        Ok(StreamEvent::Done) => {
            let mut events = Vec::new();
            if state.phase == BufferPhase::Confirmed {
                events = resolve_confirmed(state);
            } else if state.phase == BufferPhase::Probing {
                events = flush_pending(state);
            }
            events.push(Ok(StreamEvent::Done));
            events
        }
        other => vec![other],
    }
}

/// 处理 TextDelta，返回 0~N 个事件（不含 Err）。
fn handle_text_delta(state: &mut BufferState, delta: String) -> Vec<StreamEvent> {
    match state.phase {
        BufferPhase::Idle => {
            if delta.contains('<') {
                // 含 `<`：暂存并进入窗口探测。
                state.pending.push_str(&delta);
                state.phase = BufferPhase::Probing;
                check_probe_window(state)
            } else {
                vec![StreamEvent::TextDelta(delta)]
            }
        }
        BufferPhase::Probing => {
            state.pending.push_str(&delta);
            check_probe_window(state)
        }
        BufferPhase::Confirmed => {
            state.pending.push_str(&delta);
            Vec::new()
        }
    }
}

/// 完整响应结束后对确认的协议文本做最终解析。
fn resolve_confirmed(state: &mut BufferState) -> Vec<Result<StreamEvent, DeepSeekError>> {
    state.phase = BufferPhase::Idle;
    if let Some(calls) = dsml::parse_dsml_tool_calls(state.pending.trim()) {
        tracing::info!(count = calls.len(), "工具调用文本协议兜底解析出工具调用");
        let leftover = dsml::strip_tool_call_block(state.pending.trim());
        state.pending.clear();
        let mut events = Vec::new();
        if !leftover.is_empty() {
            events.push(Ok(StreamEvent::TextDelta(leftover)));
        }
        for (idx, call) in calls.into_iter().enumerate() {
            events.push(Ok(StreamEvent::TextProtocolToolCall {
                index: idx as u32,
                id: format!("textcall_{idx}"),
                name: call.name,
                arguments: call.arguments,
            }));
        }
        events
    } else {
        tracing::warn!(
            len = state.pending.len(),
            "工具调用文本缓冲解析失败，整块补发缓冲文本"
        );
        flush_pending(state)
    }
}

/// Probing 状态下检查窗口：命中关键词 → Confirmed；窗口超限 → 整块吐出回 Idle；否则继续等。
fn check_probe_window(state: &mut BufferState) -> Vec<StreamEvent> {
    // 取最后一个 `<` 之后的文本作为探测窗口。
    let lt_pos = match state.pending.rfind('<') {
        Some(pos) => pos,
        None => return flush_to_events(state),
    };
    let after_lt = &state.pending[lt_pos + 1..];
    let window_chars = after_lt.chars().count();

    if contains_protocol_prefix(&state.pending) {
        state.phase = BufferPhase::Confirmed;
        tracing::info!("DeepSeek 流式确认走工具调用文本协议，等待完整响应");
        Vec::new()
    } else if window_chars >= PROBE_WINDOW_CHARS {
        // 窗口耗尽仍无关键词 → 不是协议，整块吐出。
        flush_to_events(state)
    } else {
        // 继续等下一个 delta。
        Vec::new()
    }
}

/// 整块吐出 pending 并回 Idle。
fn flush_to_events(state: &mut BufferState) -> Vec<StreamEvent> {
    if state.pending.is_empty() {
        state.phase = BufferPhase::Idle;
        return Vec::new();
    }
    let text = std::mem::take(&mut state.pending);
    state.phase = BufferPhase::Idle;
    vec![StreamEvent::TextDelta(text)]
}

/// 整块透传 pending 并回 Idle。
fn flush_pending(state: &mut BufferState) -> Vec<Result<StreamEvent, DeepSeekError>> {
    if state.pending.is_empty() {
        state.phase = BufferPhase::Idle;
        return Vec::new();
    }
    let text = std::mem::take(&mut state.pending);
    state.phase = BufferPhase::Idle;
    vec![Ok(StreamEvent::TextDelta(text))]
}

/// 解析单个 SSE chunk，可能产出多个事件。
///
/// 同一 delta 理论上可同时携带 reasoning_content / content / tool_calls，
/// 收集全部而非只取首个非空字段。
pub(crate) fn parse_stream_chunk(data: &str) -> Vec<Result<StreamEvent, DeepSeekError>> {
    let chunk: StreamChunk = match serde_json::from_str(data) {
        Ok(chunk) => chunk,
        Err(err) => return vec![Err(DeepSeekError::Serialization(format!("{err}: {data}")))],
    };

    let mut events = Vec::new();

    if let Some(usage) = chunk.usage {
        events.push(Ok(StreamEvent::Usage(usage)));
    }

    for choice in chunk.choices {
        // OpenAI 兼容协议会发送只含 role 的首片（{"role":"assistant"}）和只含
        // finish_reason 的结束片（delta 全空）。这些是正常 chunk，不应报错。
        let delta = choice.delta;
        if let Some(reasoning) = delta.reasoning_content.filter(|s| !s.is_empty()) {
            events.push(Ok(StreamEvent::ReasoningDelta(reasoning)));
        }
        if let Some(content) = delta.content.filter(|s| !s.is_empty()) {
            events.push(Ok(StreamEvent::TextDelta(content)));
        }
        if let Some(tool_calls) = delta.tool_calls {
            for tc in tool_calls {
                if let Some(func) = tc.function.as_ref()
                    && let Some(name) = func.name.as_ref().filter(|n| !n.is_empty())
                {
                    let id = tc.id.clone().unwrap_or_default();
                    events.push(Ok(StreamEvent::ToolCallStart {
                        index: tc.index,
                        id,
                        name: name.clone(),
                    }));
                }
                if let Some(func) = tc.function
                    && let Some(args) = func.arguments.filter(|a| !a.is_empty())
                {
                    events.push(Ok(StreamEvent::ToolCallDelta {
                        index: tc.index,
                        arguments: args,
                    }));
                }
            }
        }
        if let Some(reason) = choice.finish_reason {
            events.push(Ok(StreamEvent::FinishReason(reason)));
        }
    }

    events
}

#[cfg(test)]
mod buffer_tests {
    use super::*;
    use crate::types::Usage;

    fn text_delta(s: &str) -> Result<StreamEvent, DeepSeekError> {
        Ok(StreamEvent::TextDelta(s.into()))
    }

    fn collect(events: Vec<Result<StreamEvent, DeepSeekError>>) -> Vec<String> {
        let mut state = BufferState::default();
        let mut out = Vec::new();
        for event in events {
            for mapped in apply_buffer(&mut state, event) {
                out.push(format!("{:?}", mapped.expect("应为 Ok")));
            }
        }
        out
    }

    #[test]
    fn plain_text_without_angle_bracket_passes_through() {
        // 不含 < 的纯文本逐字透传，不触发缓冲。
        let out = collect(vec![text_delta("普通回复"), Ok(StreamEvent::Done)]);
        assert!(out.iter().any(|e| e.contains("TextDelta(\"普通回复")));
        assert!(!out.iter().any(|e| e.contains("TextProtocolToolCall")));
    }

    #[test]
    fn angle_bracket_in_normal_text_flushes_after_window() {
        // 正常文本里的 < 触发缓冲，窗口满（10 字符）仍无 tool/dsml → 整块吐出。
        let out = collect(vec![
            text_delta("比较 a < b 这是正常文本无关键词"),
            Ok(StreamEvent::Done),
        ]);
        let combined: String = out
            .iter()
            .filter(|e| e.contains("TextDelta"))
            .cloned()
            .collect();
        assert!(
            combined.contains("比较 a < b 这是正常文本无关键词"),
            "正常尖括号文本应完整吐出：{out:?}"
        );
        assert!(!out.iter().any(|e| e.contains("TextProtocolToolCall")));
    }

    #[test]
    fn dsml_text_buffered_and_resolved_as_tool_call() {
        let dsml = "<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name=\"read_file\">\n<｜｜DSML｜｜parameter name=\"path\" string=\"true\">/tmp/x</｜｜DSML｜｜parameter>\n</｜｜DSML｜｜invoke>\n</｜｜DSML｜｜tool_calls>";
        let out = collect(vec![text_delta(dsml), Ok(StreamEvent::Done)]);
        assert!(
            !out.iter().any(|e| e.contains("DSML") || e.contains("｜｜")),
            "DSML 标记原文不应透传：{out:?}"
        );
        assert!(
            out.iter()
                .any(|e| e.contains("TextProtocolToolCall") && e.contains("read_file")),
            "应产出工具调用：{out:?}"
        );
    }

    #[test]
    fn dsml_preserves_explanatory_text() {
        let text = format!(
            "我来读取文件。\n{dsml}",
            dsml = "<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name=\"read_file\">\n<｜｜DSML｜｜parameter name=\"path\" string=\"true\">/tmp/x</｜｜DSML｜｜parameter>\n</｜｜DSML｜｜invoke>\n</｜｜DSML｜｜tool_calls>"
        );
        let out = collect(vec![text_delta(&text), Ok(StreamEvent::Done)]);
        assert!(
            out.iter().any(|e| e.contains("我来读取文件")),
            "说明文字应补发：{out:?}"
        );
    }

    #[test]
    fn dsml_split_across_chunks_is_detected() {
        // 分片到达：每个碎片单独不含完整关键词，但累积后窗口内出现 dsml → 确认。
        let out = collect(vec![
            text_delta("<"),
            text_delta("｜"),
            text_delta("｜DS"),
            text_delta("ML｜｜invoke name=\"fn\">"),
            text_delta(
                "<｜｜DSML｜｜parameter name=\"x\" string=\"true\">1</｜｜DSML｜｜parameter>",
            ),
            text_delta("</｜｜DSML｜｜invoke>"),
            Ok(StreamEvent::Done),
        ]);
        let leaked: Vec<_> = out
            .iter()
            .filter(|e| e.contains("DSML") || e.contains("｜｜"))
            .collect();
        assert!(leaked.is_empty(), "分片到达不应透传 DSML 碎片：{leaked:?}");
        assert!(out.iter().any(|e| e.contains("TextProtocolToolCall")));
    }

    #[test]
    fn structured_tool_call_flushes_pending_and_disables_buffering() {
        // Probing 期间出现结构化 ToolCallStart：先吐出 pending，后续不再缓冲。
        let out = collect(vec![
            text_delta("读取"),  // 无 <，正常透传
            text_delta("<未完"), // < 触发 Probing
            Ok(StreamEvent::ToolCallStart {
                index: 0,
                id: "call_0".into(),
                name: "read_file".into(),
            }),
            text_delta("正在执行"), // 结构化已出现，不再缓冲
            Ok(StreamEvent::Done),
        ]);
        assert!(
            out.iter().any(|e| e.contains("TextDelta(\"读取")),
            "缓冲前文本应透传：{out:?}"
        );
        assert!(
            out.iter().any(|e| e.contains("TextDelta(\"<未完")),
            "pending 应在 ToolCallStart 前吐出：{out:?}"
        );
        assert!(out.iter().any(|e| e.contains("ToolCallStart")));
        assert!(
            out.iter().any(|e| e.contains("TextDelta(\"正在执行")),
            "结构化工具调用后文本应正常透传：{out:?}"
        );
    }

    #[test]
    fn probing_exits_when_window_exhausted() {
        // < 后 10 字符内无 tool/dsml → 窗口耗尽整块吐出，恢复正常透传。
        let out = collect(vec![
            text_delta("前文"),
            text_delta("<abcdefghij"), // 10 字符无关键词 → 窗口耗尽吐出
            text_delta("后续正常"),    // 已回 Idle，正常透传
            Ok(StreamEvent::Done),
        ]);
        let combined: String = out
            .iter()
            .filter(|e| e.contains("TextDelta"))
            .cloned()
            .collect();
        assert!(
            combined.contains("<abcdefghij"),
            "窗口耗尽应整块吐出：{out:?}"
        );
        assert!(
            out.iter().any(|e| e.contains("TextDelta(\"后续正常")),
            "退出后应正常透传：{out:?}"
        );
        assert!(!out.iter().any(|e| e.contains("TextProtocolToolCall")));
    }

    #[test]
    fn code_block_not_infinitely_buffered() {
        // 代码块含多个 <，每个 < 后窗口内均无关键词 → 各自整块吐出。
        let out = collect(vec![
            text_delta("```html\n<div><span>hello</span></div>\n```"),
            Ok(StreamEvent::Done),
        ]);
        let combined: String = out
            .iter()
            .filter(|e| e.contains("TextDelta"))
            .cloned()
            .collect();
        assert!(
            combined.contains("<div>") && combined.contains("<span>hello</span>"),
            "代码块应完整吐出不被吞：{out:?}"
        );
        assert!(!out.iter().any(|e| e.contains("TextProtocolToolCall")));
    }

    #[test]
    fn usage_waits_for_done_before_resolution() {
        // Usage 可能先于 Done 到达，不能用它提前解析尚未结束的 content。
        let dsml = "<｜｜DSML｜｜invoke name=\"fn\">\n<｜｜DSML｜｜parameter name=\"x\" string=\"true\">1</｜｜DSML｜｜parameter>\n</｜｜DSML｜｜invoke>";
        let usage = Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens_details: None,
        };
        let mut state = BufferState::default();
        assert!(apply_buffer(&mut state, text_delta(dsml)).is_empty());
        let usage_events = apply_buffer(&mut state, Ok(StreamEvent::Usage(usage)));
        assert!(
            usage_events
                .iter()
                .any(|event| matches!(event, Ok(StreamEvent::Usage(_))))
        );
        assert!(
            usage_events
                .iter()
                .all(|event| !matches!(event, Ok(StreamEvent::TextProtocolToolCall { .. })))
        );

        let done_events = apply_buffer(&mut state, Ok(StreamEvent::Done));
        assert!(
            done_events
                .iter()
                .any(|event| matches!(event, Ok(StreamEvent::TextProtocolToolCall { .. })))
        );
    }

    #[test]
    fn complete_response_resolves_multiple_invokes_as_one_group() {
        let dsml = "<｜｜DSML｜｜tool_calls>
<｜｜DSML｜｜invoke name=\"fn_a\">
<｜｜DSML｜｜parameter name=\"x\" string=\"true\">1</｜｜DSML｜｜parameter>
</｜｜DSML｜｜invoke>
<｜｜DSML｜｜invoke name=\"fn_b\">
<｜｜DSML｜｜parameter name=\"y\" string=\"true\">2</｜｜DSML｜｜parameter>
</｜｜DSML｜｜invoke>
</｜｜DSML｜｜tool_calls>";
        let mut state = BufferState::default();
        let first_invoke_end =
            dsml.find("</｜｜DSML｜｜invoke>").unwrap() + "</｜｜DSML｜｜invoke>".len();
        assert!(
            apply_buffer(&mut state, text_delta(&dsml[..first_invoke_end])).is_empty(),
            "第一个 invoke 结束时不能提前解析"
        );
        assert!(
            apply_buffer(&mut state, text_delta(&dsml[first_invoke_end..])).is_empty(),
            "完整 content 结束前不能提前解析"
        );

        let out = apply_buffer(&mut state, Ok(StreamEvent::Done));
        let calls = out
            .iter()
            .filter_map(|event| match event {
                Ok(StreamEvent::TextProtocolToolCall { id, name, .. }) => {
                    Some((id.as_str(), name.as_str()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls, vec![("textcall_0", "fn_a"), ("textcall_1", "fn_b")]);
        assert!(
            out.iter().all(
                |event| !matches!(event, Ok(StreamEvent::TextDelta(text)) if text.contains("DSML"))
            ),
            "完整工具调用组不应泄漏协议标记：{out:?}"
        );
    }

    #[test]
    fn long_tool_arguments_are_not_downgraded_to_plain_text() {
        let value = "a".repeat(5_000);
        let dsml = format!(
            "<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name=\"write_file\">\n<｜｜DSML｜｜parameter name=\"content\" string=\"true\">{value}</｜｜DSML｜｜parameter>\n</｜｜DSML｜｜invoke>\n</｜｜DSML｜｜tool_calls>"
        );
        let out = collect(vec![text_delta(&dsml), Ok(StreamEvent::Done)]);
        assert!(
            out.iter()
                .any(|event| event.contains("TextProtocolToolCall") && event.contains("write_file")),
            "长参数仍应识别为工具调用"
        );
        assert!(
            !out.iter()
                .any(|event| event.contains("TextDelta") && event.contains("DSML")),
            "长参数工具调用不应作为普通文本透传"
        );
    }

    #[test]
    fn toolbar_text_does_not_enter_protocol_buffering() {
        let mut state = BufferState::default();
        let out = apply_buffer(
            &mut state,
            text_delta("<toolbar>普通内容继续输出，不是工具协议"),
        );
        assert!(out.iter().any(|event| matches!(
            event,
            Ok(StreamEvent::TextDelta(text)) if text.contains("<toolbar>")
        )));
        assert!(state.phase == BufferPhase::Idle);
    }
}
