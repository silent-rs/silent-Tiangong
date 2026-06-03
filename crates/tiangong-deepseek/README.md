# tiangong-deepseek

An asynchronous Rust client for the [DeepSeek API](https://api-docs.deepseek.com/).

## Features

- **Chat Completions** — synchronous and SSE streaming, with tool calling and reasoning support
- **Model Listing** — list available models
- **Balance Queries** — check account balance (CNY / USD)
- **Context Caching** — `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` exposed in usage
- **Retryable Errors** — `DeepSeekError::is_retryable()` for transport and rate-limit failures

## Quick Start

```toml
[dependencies]
tiangong-deepseek = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use tiangong_deepseek::{DeepSeekConfig, DeepSeekClient, types::*};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DeepSeekConfig::new("your-api-key");
    let client = DeepSeekClient::from_config(config)?;

    // Chat completion
    let response = client.chat().create(ChatCompletionRequest {
        model: "deepseek-chat".into(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: Some(serde_json::json!("Hello!")),
            ..Default::default()
        }],
        ..Default::default()
    }).await?;
    println!("{}", response.choices[0].message.content.unwrap_or_default());

    // Streaming
    let stream = client.chat().create_stream(ChatCompletionRequest {
        model: "deepseek-chat".into(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: Some(serde_json::json!("Explain Rust in one sentence.")),
            ..Default::default()
        }],
        stream: Some(true),
        ..Default::default()
    }).await?;

    use futures_util::StreamExt;
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::TextDelta(text) => print!("{text}"),
            StreamEvent::Done => println!(),
            _ => {}
        }
    }

    Ok(())
}
```

> `ChatMessage` fields like `name`, `tool_calls`, `tool_call_id`, and `prefix` default to `None`/`false` via `#[serde(default)]`. You can use `..Default::default()` to omit them.

## API Coverage

| Endpoint | Method | Status |
|----------|--------|--------|
| `/chat/completions` | POST | Supported (sync + SSE stream) |
| `/models` | GET | Supported |
| `/user/balance` | GET | Supported |

## License

Apache License 2.0
