use eventsource_stream::Eventsource;
use futures_util::{StreamExt, stream};

use crate::client::DeepSeekClient;
use crate::error::DeepSeekError;
use crate::types::{
    CreateResponseRequest, ResponseObject, ResponsesEventStream, ResponsesStreamEvent,
};

pub struct Responses<'c> {
    client: &'c DeepSeekClient,
}

impl<'c> Responses<'c> {
    pub fn new(client: &'c DeepSeekClient) -> Self {
        Self { client }
    }

    /// 创建非流式响应。
    ///
    /// Responses API 为无状态设计：服务端不存储会话，多轮对话需在
    /// `input` 中回传完整历史。输入超出上下文窗口时服务端直接返回 400。
    ///
    /// 无论调用方传入什么 `stream` 值，此方法始终强制关闭流式，
    /// 保证返回值按普通 JSON 解析。
    pub async fn create(
        &self,
        mut request: CreateResponseRequest,
    ) -> Result<ResponseObject, DeepSeekError> {
        request.stream = Some(false);
        self.client.post("/responses", &request).await
    }

    /// 创建流式响应。
    ///
    /// DeepSeek 不发送 `[DONE]` 标记；终止事件
    /// （`response.completed` / `response.incomplete` / `response.failed`）
    /// 产出后流即结束，其余后续消息被忽略。
    pub async fn create_stream(
        &self,
        mut request: CreateResponseRequest,
    ) -> Result<ResponsesEventStream, DeepSeekError> {
        request.stream = Some(true);
        let response = self.client.post_stream_raw("/responses", &request).await?;

        let sse_stream = response.bytes_stream().eventsource();
        let event_stream = stream::unfold(
            (sse_stream, false),
            |(mut sse_stream, terminated)| async move {
                if terminated {
                    return None;
                }
                match sse_stream.next().await {
                    Some(Ok(message)) => {
                        let event = parse_stream_event(&message.data);
                        let is_terminal = matches!(
                            &event,
                            Ok(ResponsesStreamEvent::ResponseCompleted { .. }
                                | ResponsesStreamEvent::ResponseIncomplete { .. }
                                | ResponsesStreamEvent::ResponseFailed { .. })
                        );
                        Some((event, (sse_stream, is_terminal)))
                    }
                    Some(Err(err)) => Some((
                        Err(DeepSeekError::Stream(err.to_string())),
                        (sse_stream, true),
                    )),
                    None => None,
                }
            },
        );

        Ok(Box::pin(event_stream))
    }
}

/// 解析单条 SSE data 为流事件。
///
/// 未知事件类型（服务端新增而 SDK 尚未支持）降级为
/// `ResponsesStreamEvent::Unknown` 透传，不中断流。
pub(crate) fn parse_stream_event(data: &str) -> Result<ResponsesStreamEvent, DeepSeekError> {
    let value: serde_json::Value = serde_json::from_str(data)
        .map_err(|err| DeepSeekError::Serialization(format!("{err}: {data}")))?;
    let event_type = value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    match serde_json::from_value::<ResponsesStreamEvent>(value) {
        Ok(event) => Ok(event),
        Err(err) => {
            tracing::warn!(
                event_type,
                error = %err,
                "无法解析的 Responses 流事件，按未知事件透传"
            );
            Ok(ResponsesStreamEvent::Unknown { event_type })
        }
    }
}
