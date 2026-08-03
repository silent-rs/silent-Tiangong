use eventsource_stream::Eventsource;
use futures_util::{StreamExt, stream};

use crate::client::DeepSeekClient;
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

    pub async fn create_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<EventStream, DeepSeekError> {
        let response = self
            .client
            .post_stream_raw("/chat/completions", &request)
            .await?;

        let sse_stream = response.bytes_stream().eventsource();
        // 单个 SSE message 可能产出多个事件（同一 delta 同时含 reasoning/content/tool_calls），
        // 这里按 Vec 产出再 flat_map 展平。
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
        Ok(Box::pin(chunk_stream.flat_map(stream::iter)))
    }
}

/// 解析单个 SSE chunk，可能产出多个事件。
///
/// 同一 delta 理论上可同时携带 reasoning_content / content / tool_calls，
/// 此前用 if-return 级联只会产出首个非空字段，其余被丢弃。现在改为收集全部。
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
    }

    // 兜底：chunk 完全无法产出任何事件时返回错误，保持与旧行为一致以便上层感知异常。
    if events.is_empty() {
        vec![Err(DeepSeekError::Stream(format!(
            "failed to parse stream chunk: {data}"
        )))]
    } else {
        events
    }
}
