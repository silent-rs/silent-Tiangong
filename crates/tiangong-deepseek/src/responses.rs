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
        // 建连与等待响应头受用户配置的请求超时约束，避免网关建连后
        // 不返回响应头导致永久等待；SSE 建流成功后不受总时限限制。
        let timeout = self.client.stream_timeout();
        let mut response =
            tokio::time::timeout(timeout, self.client.post_stream_raw("/responses", &request))
                .await
                .map_err(|_| DeepSeekError::Timeout(format!("{} ms", timeout.as_millis())))??;

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();

        if !content_type.contains("text/event-stream") {
            // 类型不明确（部分网关漏标或错标）时按首块内容探测。
            let first = response
                .chunk()
                .await
                .map_err(|err| DeepSeekError::Transport(err.to_string()))?
                .unwrap_or_default();
            if content_type.contains("json") || looks_like_json(&first) {
                // 网关忽略 stream 参数返回一次性 JSON：SSE 解析器会把这些行全部当
                // 未知字段丢弃且不报错，在 SDK 内按完整响应接住并合成事件流。
                // 完整读取受超时约束，避免迟迟不结束的响应永久等待。
                let complete: ResponseObject = crate::client::read_complete_response(
                    response,
                    first,
                    self.client.stream_timeout(),
                )
                .await?;
                return Ok(Box::pin(stream::iter(
                    complete_response_events(complete).into_iter().map(Ok),
                )));
            }
            // 首块不是 JSON：按 SSE 流解析，把首块拼回流头。
            let byte_stream = stream::once(async move { Ok::<_, reqwest::Error>(first) })
                .chain(response.bytes_stream());
            return Ok(Box::pin(responses_sse_event_stream(byte_stream)));
        }

        Ok(Box::pin(responses_sse_event_stream(
            response.bytes_stream(),
        )))
    }
}

/// SSE 字节流转 Responses 流事件；终止事件后流结束。
fn responses_sse_event_stream<S>(
    byte_stream: S,
) -> impl futures_util::Stream<Item = Result<ResponsesStreamEvent, DeepSeekError>>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    use eventsource_stream::Eventsource;
    use futures_util::StreamExt;
    let sse_stream = Box::pin(byte_stream.eventsource());
    stream::unfold(
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
    )
}

/// 判断响应首块是否是 JSON（跳过空白后以 `{` 或 `[` 开头）。
fn looks_like_json(first: &[u8]) -> bool {
    first
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'{' || *byte == b'[')
}

/// 把一次性完整响应合成为等价的流事件序列。
///
/// 网关忽略 stream 参数返回 application/json 时，SDK 在内部按完整响应
/// 接住并合成事件流，调用方无需感知差异。ResponseCompleted 为终止事件，
/// 流在其后自然结束。
fn complete_response_events(response: ResponseObject) -> Vec<ResponsesStreamEvent> {
    vec![
        ResponsesStreamEvent::ResponseCreated {
            sequence_number: 0,
            response: response.clone(),
        },
        ResponsesStreamEvent::ResponseCompleted {
            sequence_number: 1,
            response,
        },
    ]
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
