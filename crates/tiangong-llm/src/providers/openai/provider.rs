use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures_util::{Stream, StreamExt, stream};
use serde_json::Value;

use crate::error::LlmError;
use crate::model::ProviderModelInfo;
use crate::provider::{LlmProvider, ProviderCapabilities};
use crate::request::ProviderRequest;
use crate::response::ProviderResponse;
use crate::stream::{ProviderStream, ProviderStreamEvent};

use super::client::ResponsesClient;
use super::config::OpenAiResponsesConfig;
use super::error::map_responses_error;
use super::mapping::{build_request_json, parse_complete_response};
use super::stream::{ResponsesStreamParser, extract_completed_reasoning};

struct BackgroundResponsesStream {
    inner: ProviderStream,
    client: ResponsesClient,
    response_id: Arc<Mutex<Option<String>>>,
    terminal: Arc<AtomicBool>,
}

impl Stream for BackgroundResponsesStream {
    type Item = Result<ProviderStreamEvent, LlmError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl Drop for BackgroundResponsesStream {
    fn drop(&mut self) {
        if self.terminal.load(Ordering::Acquire) {
            return;
        }
        let response_id = self
            .response_id
            .lock()
            .ok()
            .and_then(|response_id| response_id.clone());
        let Some(response_id) = response_id else {
            return;
        };
        let client = self.client.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(%response_id, "无法取得异步运行时，跳过 Responses 服务端取消");
            return;
        };
        handle.spawn(async move {
            if let Err(error) = client.cancel(&response_id).await {
                tracing::warn!(%response_id, %error, "Responses 服务端取消失败");
            } else {
                tracing::info!(%response_id, "Responses 服务端取消成功");
            }
        });
    }
}

#[derive(Clone)]
pub struct OpenAiResponsesProvider {
    client: ResponsesClient,
}

impl OpenAiResponsesProvider {
    pub fn new(config: OpenAiResponsesConfig) -> Self {
        Self {
            client: ResponsesClient::new(config),
        }
    }
}

/// 对 completed 事件的 reasoning 兜底做条件去重。
///
/// OpenAI Responses 的思考内容主要靠流式增量事件（`reasoning_summary_text.delta` 等）
/// 多段拼接，这些增量**必须全部保留**。只有当整个流式过程**完全没有收到任何 reasoning
/// delta** 时，才从 `response.completed` 的最终 `output[]` 中兜底提取 reasoning。
///
/// 因此去重只针对 completed 兜底：`received_delta` 为 true 时跳过兜底 reasoning。
fn maybe_completed_reasoning(
    completed_payload: &Value,
    received_delta: bool,
) -> Option<ProviderStreamEvent> {
    if received_delta {
        return None;
    }
    extract_completed_reasoning(completed_payload)
        .filter(|text| !text.trim().is_empty())
        .map(ProviderStreamEvent::ReasoningDelta)
}

#[async_trait]
impl LlmProvider for OpenAiResponsesProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            system_prompt: true,
            list_models: true,
        }
    }

    async fn complete(&self, req: ProviderRequest) -> Result<ProviderResponse, LlmError> {
        let model = req.model.clone();
        let payload = build_request_json(&req, false)
            .map_err(|err| LlmError::InvalidRequest(err.to_string()))?;
        let response = self.client.complete(&model, payload).await?;
        parse_complete_response(&response).map_err(|err| LlmError::Provider {
            provider: "openai",
            message: err.to_string(),
        })
    }

    async fn stream(&self, req: ProviderRequest) -> Result<ProviderStream, LlmError> {
        let model = req.model.clone();
        let payload = build_request_json(&req, true)
            .map_err(|err| LlmError::InvalidRequest(err.to_string()))?;
        let stream = self.client.stream(&model, payload).await?;
        let response_id = Arc::new(Mutex::new(None));
        let terminal = Arc::new(AtomicBool::new(false));
        let stream_response_id = Arc::clone(&response_id);
        let stream_terminal = Arc::clone(&terminal);

        // 维护流式状态：记录是否收到过 reasoning delta 增量。
        // 流式增量一律保留；仅在全程无 delta 时，从 completed 兜底补发 reasoning。
        let mut received_reasoning_delta = false;
        let mut parser = ResponsesStreamParser::default();
        let mapped = stream.flat_map(move |item| {
            let raw_payload = match item {
                Ok(payload) => payload,
                Err(err) => {
                    return stream::iter(vec![Err(map_responses_error(&err))]);
                }
            };

            let event_type = raw_payload
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("");
            if event_type == "response.created"
                && let Some(id) = raw_payload
                    .get("response")
                    .and_then(|response| response.get("id"))
                    .and_then(Value::as_str)
                && let Ok(mut stored_id) = stream_response_id.lock()
            {
                *stored_id = Some(id.to_string());
            }
            if matches!(
                event_type,
                "response.completed" | "response.failed" | "response.incomplete"
            ) {
                stream_terminal.store(true, Ordering::Release);
            }

            // 记录本条事件是否产生 reasoning delta（用于 completed 兜底判断）。
            let is_completed = raw_payload
                .get("type")
                .and_then(Value::as_str)
                .map(|t| t == "response.completed")
                .unwrap_or(false);

            let mut events = parser.parse_event(&raw_payload);

            if is_completed {
                // 仅当流式过程未收到任何 reasoning delta 时，兜底补发最终 reasoning。
                if let Some(fallback) =
                    maybe_completed_reasoning(&raw_payload, received_reasoning_delta)
                {
                    events.insert(0, Ok(fallback));
                }
            } else if events
                .iter()
                .any(|e| matches!(e, Ok(ProviderStreamEvent::ReasoningDelta(_))))
            {
                received_reasoning_delta = true;
            }

            stream::iter(events)
        });
        let inner: ProviderStream = Box::pin(mapped);
        Ok(Box::pin(BackgroundResponsesStream {
            inner,
            client: self.client.clone(),
            response_id,
            terminal,
        }))
    }

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, LlmError> {
        self.client.list_models().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_background_stream_cancels_response_on_server() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_json(serde_json::json!({
                "model": "gpt-5.6-sol",
                "input": [{"type": "message", "role": "user", "content": "你好"}],
                "stream": true,
                "background": true,
                "max_output_tokens": 1024
            })))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_test\"}}\n\n",
                "text/event-stream",
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses/resp_test/cancel"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_test",
                "status": "cancelled"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = OpenAiResponsesConfig::new("test-key", server.uri());
        config.timeout = std::time::Duration::from_secs(2);
        config.max_retries = 0;
        let provider = OpenAiResponsesProvider::new(config);
        let request = ProviderRequest {
            model: "gpt-5.6-sol".to_string(),
            system: None,
            messages: vec![crate::message::ChatMessage::text(
                crate::message::MessageRole::User,
                "你好",
            )],
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: 1024,
            temperature: None,
            top_p: None,
            stop_sequences: Vec::new(),
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            thinking_disabled: false,
        };

        let mut response_stream = provider.stream(request).await.unwrap();
        assert!(matches!(
            response_stream.next().await,
            Some(Ok(ProviderStreamEvent::MessageStart))
        ));
        drop(response_stream);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        server.verify().await;
    }

    #[test]
    fn fallback_emitted_when_no_delta_received() {
        let completed = serde_json::json!({
            "response": {
                "output": [
                    { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "最终思考" }] }
                ]
            }
        });
        let event = maybe_completed_reasoning(&completed, false);
        let ok = matches!(
            event.as_ref(),
            Some(ProviderStreamEvent::ReasoningDelta(t)) if t == "最终思考"
        );
        assert!(ok, "无 delta 时应兜底: {event:?}");
    }

    #[test]
    fn fallback_skipped_when_delta_received() {
        // 已通过流式 delta 收到思考，completed 兜底应跳过，避免重复（关键 bug 修复）。
        let completed = serde_json::json!({
            "response": {
                "output": [
                    { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "最终思考" }] }
                ]
            }
        });
        let event = maybe_completed_reasoning(&completed, true);
        assert!(event.is_none(), "已收到 delta 时不应兜底重复: {event:?}");
    }

    #[test]
    fn fallback_none_when_completed_has_no_reasoning() {
        let completed = serde_json::json!({
            "response": {
                "output": [{ "type": "message", "content": [{ "type": "output_text", "text": "答案" }] }]
            }
        });
        assert!(maybe_completed_reasoning(&completed, false).is_none());
    }
}
