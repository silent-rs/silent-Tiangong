use futures_util::{StreamExt, stream};

use crate::error::LlmError;
use crate::stream::ProviderStreamEvent;
use crate::tool::ToolCall;

use super::dsml;
use super::error::map_deepseek_error;
use super::mapping::parse_stream_usage;

/// 流式映射的内部状态：累积文本、判断是否进入工具调用文本协议模式。
#[derive(Default)]
struct StreamState {
    /// 累积的正文文本。
    text: String,
    /// 是否已经出现过结构化的 ToolCallStart（出现则说明本条不走文本协议）。
    has_structured_tool_call: bool,
    /// 是否进入缓冲模式（已观察到文本协议特征前缀，待流末最终判定）。
    buffering: bool,
}

pub fn map_deepseek_stream(
    event_stream: tiangong_deepseek::types::EventStream,
) -> super::client::DeepSeekStream {
    let state = StreamState::default();

    // scan 在透传事件的同时维护状态；在流末（Usage/Done）根据累积文本做兜底。
    let mapped = event_stream.scan(state, |state, result| {
        let events: Vec<Result<ProviderStreamEvent, LlmError>> = match result {
            Ok(event) => map_event_with_state(state, event),
            Err(err) => vec![Err(map_deepseek_error(err))],
        };
        std::future::ready(Some(events))
    });

    Box::pin(mapped.flat_map(stream::iter))
}

fn map_event_with_state(
    state: &mut StreamState,
    event: tiangong_deepseek::types::StreamEvent,
) -> Vec<Result<ProviderStreamEvent, LlmError>> {
    match event {
        tiangong_deepseek::types::StreamEvent::ReasoningDelta(delta) => {
            vec![Ok(ProviderStreamEvent::ReasoningDelta(delta))]
        }
        tiangong_deepseek::types::StreamEvent::TextDelta(delta) => {
            state.text.push_str(&delta);
            // 仅当未出现结构化工具调用时，才考察是否进入缓冲。
            if !state.has_structured_tool_call {
                // 出现任一已知工具调用文本协议的特征前缀即提前缓冲，
                // 避免标记碎片作为 TextDelta 透传给用户（含分片到达场景）。
                if !state.buffering && dsml::looks_like_tool_call_text(&state.text) {
                    state.buffering = true;
                    tracing::info!(
                        accumulated_len = state.text.len(),
                        "DeepSeek 流式疑似走工具调用文本协议，开始缓冲"
                    );
                }
            }
            // 缓冲期间不透传文本；其余正常透传。
            if state.buffering {
                Vec::new()
            } else {
                vec![Ok(ProviderStreamEvent::TextDelta(delta))]
            }
        }
        tiangong_deepseek::types::StreamEvent::ToolCallStart { id, name } => {
            state.has_structured_tool_call = true;
            vec![Ok(ProviderStreamEvent::ToolCallStart(ToolCall {
                id,
                name,
                arguments: serde_json::json!({}),
            }))]
        }
        tiangong_deepseek::types::StreamEvent::ToolCallDelta { index, arguments } => {
            vec![Ok(ProviderStreamEvent::ToolCallDelta {
                call_id: index.to_string(),
                partial_json: arguments,
            })]
        }
        tiangong_deepseek::types::StreamEvent::Usage(usage) => {
            let mut events = vec![Ok(ProviderStreamEvent::Usage(parse_stream_usage(&usage)))];
            if state.buffering {
                events.extend(resolve_buffered_tool_calls(state));
            }
            events
        }
        tiangong_deepseek::types::StreamEvent::Done => {
            let mut events = Vec::new();
            if state.buffering {
                events = resolve_buffered_tool_calls(state);
            }
            events.push(Ok(ProviderStreamEvent::MessageEnd));
            events
        }
        tiangong_deepseek::types::StreamEvent::Error(message) => {
            vec![Err(LlmError::Provider {
                provider: "deepseek",
                message,
            })]
        }
    }
}

/// 流末对缓冲文本做最终判定：
/// - 确认是完整工具调用文本块 → 解析为 ToolCall 事件序列（原生协议或 DSML 协议）；
/// - 否则视为误判 → 把缓冲文本原样补发，避免吞掉用户可见回复。
fn resolve_buffered_tool_calls(
    state: &mut StreamState,
) -> Vec<Result<ProviderStreamEvent, LlmError>> {
    if let Some(calls) = dsml::parse_dsml_tool_calls(state.text.trim()) {
        tracing::info!(count = calls.len(), "工具调用文本协议兜底解析出工具调用");
        state.buffering = false;
        // 先补发标记外的说明文字（如"我来读取文件"），与非流式行为一致。
        let leftover = dsml::strip_tool_call_block(state.text.trim());
        let mut events = Vec::new();
        if !leftover.is_empty() {
            events.push(Ok(ProviderStreamEvent::TextDelta(leftover)));
        }
        events.extend(calls.into_iter().enumerate().flat_map(|(idx, call)| {
            let call_id = format!("textcall_{idx}");
            [
                Ok(ProviderStreamEvent::ToolCallStart(ToolCall {
                    id: call_id.clone(),
                    name: call.name,
                    arguments: serde_json::json!({}),
                })),
                Ok(ProviderStreamEvent::ToolCallDelta {
                    call_id: call_id.clone(),
                    partial_json: call.arguments,
                }),
                Ok(ProviderStreamEvent::ToolCallEnd { call_id }),
            ]
        }));
        return events;
    }
    // 误判：把缓冲文本原样补发，不吞内容。
    tracing::warn!(
        accumulated_len = state.text.len(),
        "工具调用文本缓冲判定为误判，补发缓冲文本"
    );
    state.buffering = false;
    if state.text.trim().is_empty() {
        Vec::new()
    } else {
        vec![Ok(ProviderStreamEvent::TextDelta(std::mem::take(
            &mut state.text,
        )))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    type DsEvent = tiangong_deepseek::types::StreamEvent;
    type DsError = tiangong_deepseek::error::DeepSeekError;

    async fn collect_events(events: Vec<DsEvent>) -> Vec<String> {
        let input: Vec<Result<DsEvent, DsError>> = events.into_iter().map(Ok).collect();
        let mut stream = map_deepseek_stream(Box::pin(futures_util::stream::iter(input)));
        let mut out = Vec::new();
        while let Some(Ok(event)) = stream.next().await {
            out.push(format!("{event:?}"));
        }
        out
    }

    #[tokio::test]
    async fn structured_tool_calls_pass_through() {
        let events = collect_events(vec![
            DsEvent::TextDelta("正在读取".into()),
            DsEvent::ToolCallStart {
                id: "call_0".into(),
                name: "read_file".into(),
            },
            DsEvent::Done,
        ])
        .await;
        assert!(events.iter().any(|e| e.contains("TextDelta(\"正在读取")));
        assert!(events.iter().any(|e| e.contains("ToolCallStart")));
    }

    #[tokio::test]
    async fn dsml_text_is_buffered_and_drained_as_tool_calls() {
        let dsml_text = "<｜｜DSML｜｜tool_calls>
<｜｜DSML｜｜invoke name=\"read_file\">
<｜｜DSML｜｜parameter name=\"path\" string=\"true\">/tmp/x</｜｜DSML｜｜parameter>
</｜｜DSML｜｜invoke>
</｜｜DSML｜｜tool_calls>";
        let events =
            collect_events(vec![DsEvent::TextDelta(dsml_text.into()), DsEvent::Done]).await;
        assert!(
            !events
                .iter()
                .any(|e| e.contains("DSML") || e.contains("｜｜")),
            "DSML 标记原文不应透传：{events:?}"
        );
        assert!(
            events.iter().any(|e| e.contains("ToolCallStart")),
            "应补发工具调用：{events:?}"
        );
    }

    #[tokio::test]
    async fn dsml_preserves_explanatory_text() {
        // 工具调用前的说明文字应在兜底解析后作为 TextDelta 补发，不丢失。
        let text = format!(
            "我来读取配置文件。\n{dsml}",
            dsml = "<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name=\"read_file\">\n<｜｜DSML｜｜parameter name=\"path\" string=\"true\">/tmp/x</｜｜DSML｜｜parameter>\n</｜｜DSML｜｜invoke>\n</｜｜DSML｜｜tool_calls>"
        );
        let events = collect_events(vec![DsEvent::TextDelta(text), DsEvent::Done]).await;
        assert!(
            events.iter().any(|e| e.contains("我来读取配置文件")),
            "说明文字应被补发：{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e.contains("ToolCallStart") && e.contains("read_file")),
            "应解析出工具调用：{events:?}"
        );
    }

    #[tokio::test]
    async fn native_text_is_buffered_and_drained_as_tool_calls() {
        let native = format!(
            "{cb}{cbb}function{sep}get_weather\n```json\n{{\"city\": \"北京\"}}\n```\n{ce}{cbe}",
            cb = "<｜tool▁calls▁begin｜>",
            cbe = "<｜tool▁calls▁end｜>",
            cbb = "<｜tool▁call▁begin｜>",
            ce = "<｜tool▁call▁end｜>",
            sep = "<｜tool▁sep｜>",
        );
        let events = collect_events(vec![DsEvent::TextDelta(native), DsEvent::Done]).await;
        assert!(
            !events
                .iter()
                .any(|e| e.contains("tool▁call") || e.contains("calls▁begin")),
            "原生标记原文不应透传：{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e.contains("ToolCallStart") && e.contains("get_weather"))
        );
    }

    #[tokio::test]
    async fn split_across_chunks_is_detected() {
        // 标记分片到达：单独到达时检测不到完整标记，但特征前缀触发缓冲。
        let events = collect_events(vec![
            DsEvent::TextDelta("<｜".into()),
            DsEvent::TextDelta("｜DSML｜｜tool_calls>".into()),
            DsEvent::TextDelta("<｜｜DSML｜｜invoke name=\"run_shell\">".into()),
            DsEvent::TextDelta(
                "<｜｜DSML｜｜parameter name=\"script\" string=\"true\">ls</｜｜DSML｜｜parameter>"
                    .into(),
            ),
            DsEvent::TextDelta("</｜｜DSML｜｜invoke>".into()),
            DsEvent::TextDelta("</｜｜DSML｜｜tool_calls>".into()),
            DsEvent::Done,
        ])
        .await;
        assert!(
            !events
                .iter()
                .any(|e| e.contains("DSML") || e.contains("｜｜")),
            "分片到达时也不应透传标记碎片：{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e.contains("ToolCallStart") && e.contains("run_shell")),
            "应解析出 run_shell 工具调用：{events:?}"
        );
    }

    #[tokio::test]
    async fn false_positive_hint_reflushes_text() {
        // 普通文本恰好含工具调用特征前缀，但实际不是 → 流末应把文本补发。
        // 注：<｜tool▁call▁begin｜> 含完整 sep 前缀，需构造一个真正无法解析的碎片。
        let events = collect_events(vec![
            DsEvent::TextDelta("前缀<｜tool▁sep｜>后缀".into()),
            DsEvent::Done,
        ])
        .await;
        assert!(
            events
                .iter()
                .any(|e| e.contains("前缀") && e.contains("后缀")),
            "误判时应补发文本：{events:?}"
        );
        assert!(!events.iter().any(|e| e.contains("ToolCallStart")));
    }

    #[tokio::test]
    async fn plain_text_passes_through() {
        let events =
            collect_events(vec![DsEvent::TextDelta("普通回复".into()), DsEvent::Done]).await;
        assert!(events.iter().any(|e| e.contains("TextDelta")));
        assert!(!events.iter().any(|e| e.contains("ToolCallStart")));
    }
}
