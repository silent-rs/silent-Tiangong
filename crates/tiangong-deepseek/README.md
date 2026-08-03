# tiangong-deepseek

An asynchronous Rust client for the [DeepSeek API](https://api-docs.deepseek.com/).

## Features

- **Chat Completions** — synchronous and SSE streaming, with tool calling and reasoning support
- **Thinking Mode** — `thinking`（enabled/disabled）+ `reasoning_effort`（low/high/max）分档控制，`reasoning_content` 思考内容解析
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
        model: "deepseek-v4-pro".into(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: Some(serde_json::json!("Hello!")),
            ..Default::default()
        }],
        ..Default::default()
    }).await?;
    println!("{}", response.choices[0].message.content.unwrap_or_default());

    // Streaming with thinking mode + usage
    let stream = client.chat().create_stream(ChatCompletionRequest {
        model: "deepseek-v4-pro".into(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: Some(serde_json::json!("Explain Rust in one sentence.")),
            ..Default::default()
        }],
        stream: Some(true),
        stream_options: Some(StreamOptions { include_usage: true }),
        thinking: Some(ThinkingConfig { thinking_type: "enabled".into() }),
        reasoning_effort: Some(ReasoningEffort::High),
        ..Default::default()
    }).await?;

    use futures_util::StreamExt;
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::ReasoningDelta(text) => eprint!("\r[思考] {text}"),
            StreamEvent::TextDelta(text) => print!("{text}"),
            StreamEvent::Usage(usage) => {
                let hit = usage.prompt_cache_hit_tokens.unwrap_or(0);
                println!("\n[kv cache 命中 {hit} tokens]");
            }
            StreamEvent::Done => println!(),
            _ => {}
        }
    }

    Ok(())
}
```

> `ChatMessage` fields like `name`, `tool_calls`, `tool_call_id`, and `prefix` default to `None`/`false` via `#[serde(default)]`. You can use `..Default::default()` to omit them.
>
> 当前模型为 `deepseek-v4-pro`（满配）与 `deepseek-v4-flash`（轻量），两者均支持思考模式与工具调用。

## Thinking Mode

DeepSeek V4 默认开启思考模式，通过 `reasoning_content` 字段返回思维链。开关与强度控制：

| 字段 | 取值 | 说明 |
|------|------|------|
| `thinking.type` | `"enabled"` / `"disabled"` | 开关思考模式 |
| `reasoning_effort` | `Low` / `High` / `Max` | 思考强度（v4-flash 三档，v4-pro 暂支持 High/Max） |

注意事项：
- 思考模式下 `temperature`/`top_p` 不生效，可不传。
- **多轮工具调用**时，assistant 消息中的 `reasoning_content` 必须随后续请求完整回传，否则 API 返回 400。
- 普通多轮对话（无工具调用）的 `reasoning_content` 无需回传。

## API Coverage

| Endpoint | Method | Status |
|----------|--------|--------|
| `/chat/completions` | POST | Supported (sync + SSE stream, thinking mode, tool calling) |
| `/models` | GET | Supported |
| `/user/balance` | GET | Supported |

## License

Apache License 2.0
