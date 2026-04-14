use futures_util::{StreamExt, stream};

use crate::error::LlmError;
use crate::stream::{ProviderStream, ProviderStreamEvent};

use super::mapping::{AnthropicStreamState, map_stream_error, map_stream_event};

pub(crate) type AnthropicSdkStream = async_anthropic::types::CreateMessagesResponseStream;

pub(super) fn map_anthropic_stream(stream_in: AnthropicSdkStream) -> ProviderStream {
    let mut state = AnthropicStreamState::default();
    let stream = stream_in
        .map(move |event| match event {
            Ok(event) => match map_stream_event(&mut state, event) {
                Ok(events) => events
                    .into_iter()
                    .map(Ok)
                    .collect::<Vec<Result<ProviderStreamEvent, LlmError>>>(),
                Err(err) => map_stream_error(err),
            },
            Err(err) => map_stream_error(super::error::map_anthropic_error(err)),
        })
        .flat_map(stream::iter);
    Box::pin(stream)
}
