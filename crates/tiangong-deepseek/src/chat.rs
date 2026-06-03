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
        let stream = stream::unfold(sse_stream, |mut sse_stream| async move {
            match sse_stream.next().await {
                Some(Ok(message)) => {
                    if message.data == "[DONE]" {
                        return Some((Ok(StreamEvent::Done), sse_stream));
                    }
                    let event = parse_stream_chunk(&message.data);
                    Some((event, sse_stream))
                }
                Some(Err(err)) => Some((Err(DeepSeekError::Stream(err.to_string())), sse_stream)),
                None => None,
            }
        });
        Ok(Box::pin(stream))
    }
}

pub(crate) fn parse_stream_chunk(data: &str) -> Result<StreamEvent, DeepSeekError> {
    let chunk: StreamChunk = serde_json::from_str(data)
        .map_err(|err| DeepSeekError::Serialization(format!("{err}: {data}")))?;

    if let Some(usage) = chunk.usage {
        return Ok(StreamEvent::Usage(usage));
    }

    for choice in chunk.choices {
        let delta = choice.delta;

        if let Some(reasoning) = delta.reasoning_content.filter(|s| !s.is_empty()) {
            return Ok(StreamEvent::ReasoningDelta(reasoning));
        }
        if let Some(content) = delta.content.filter(|s| !s.is_empty()) {
            return Ok(StreamEvent::TextDelta(content));
        }
        if let Some(tool_calls) = delta.tool_calls {
            for tc in tool_calls {
                if let Some(func) = tc.function.as_ref()
                    && let Some(name) = func.name.as_ref().filter(|n| !n.is_empty())
                {
                    let id = tc.id.clone().unwrap_or_default();
                    return Ok(StreamEvent::ToolCallStart {
                        id,
                        name: name.clone(),
                    });
                }
                if let Some(func) = tc.function
                    && let Some(args) = func.arguments.filter(|a| !a.is_empty())
                {
                    return Ok(StreamEvent::ToolCallDelta {
                        index: tc.index,
                        arguments: args,
                    });
                }
            }
        }
    }

    Err(DeepSeekError::Stream(format!(
        "failed to parse stream chunk: {data}"
    )))
}
