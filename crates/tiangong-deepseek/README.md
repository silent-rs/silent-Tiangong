# tiangong-deepseek

An asynchronous Rust client for the [DeepSeek API](https://api-docs.deepseek.com/).

## Features

- **Chat Completions** — synchronous and SSE streaming, with tool calling and reasoning support
- **Thinking Mode** — `thinking`（enabled/disabled）+ `reasoning_effort`（low/high/max）分档控制，`reasoning_content` 思考内容解析
- **Text Protocol Fallback** — 内置工具调用文本协议兜底（原生 + DSML），SDK 自动从 `content` 文本中识别并解析
- **Streaming Robustness** — 单 chunk 多事件收集、空 delta 容错
- **Model Listing** — list available models
- **Balance Queries** — check account balance (CNY / USD)
- **Context Caching** — `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` exposed in usage
- **Retryable Errors** — `DeepSeekError::is_retryable()` for transport and rate-limit failures

## Quick Start

```toml
[dependencies]
tiangong-deepseek = "0.1.1"
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
            StreamEvent::TextProtocolToolCall { name, arguments, .. } => {
                println!("\n[文本协议工具调用] {name}: {arguments}");
            }
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

## 工具调用文本协议兜底

正常情况下 DeepSeek 通过结构化 `tool_calls` 字段返回工具调用。但部分场景下模型会把工具调用写进 `content` 文本，使用特殊标记协议。**SDK 内置完整兜底**，调用方无需自行处理。

### 触发条件

仅当请求携带 `tools` 时启用。无 tools 时（如总结阶段 `ToolChoice::None`）模型不可能走工具调用，兜底关闭，避免误伤讨论协议的正常文本。

### 工作方式

**流式**（`create_stream`）：SDK 在接收流式响应的同时收集文本，使用三态状态机判定：
- `Idle` → 遇到 `<` 进入探测
- `Probing` → 窗口内（10 字符）匹配协议精确前缀（`｜tool` / `｜｜dsml`）→ `Confirmed`；窗口满未命中 → 整块吐出回 `Idle`
- `Confirmed` → 持续缓冲，完整响应结束后统一解析为 `StreamEvent::TextProtocolToolCall`

探测使用精确前缀而非宽泛关键词，避免把 `<toolbar>`、`<dsml>` 等普通内容误判为工具调用。

**非流式**：`tiangong_deepseek::dsml::parse_dsml_tool_calls(content)` 从文本提取工具调用，`strip_tool_call_block(content)` 剥离标记文本。

### 底层支撑

- **流式 chunk 多事件输出**：单个 SSE chunk 可能同时携带 `reasoning_content`/`content`/`tool_calls`，收集全部而非只取首个非空字段。
- **空 delta 容错**：只含 `role` 的首片和只含 `finish_reason` 的结束片（delta 全空）静默跳过，不报错。

### 已知协议

| 协议 | 前缀标记 | 来源 |
|------|----------|------|
| 原生协议 | `<｜tool▁calls▁begin｜>` 等（竖线 U+FF5C，分隔符 ▁ U+2581） | tokenizer 内置原子 token（id 128806~128814），完整出现 |
| DSML 协议 | `<｜｜DSML｜｜invoke>` 等（双全角竖线） | V3.2 引入的 XML 风格，非原子 token，流式时分片到达 |

原生协议标记是原子 token，模型输出时完整出现，解析采用严格匹配。DSML 协议标记是普通字符流，流式时按字符分片到达，外层包裹可能残缺或完全缺失——采用部分识别策略，只要出现内层 `<｜｜DSML｜｜invoke` 标记就尝试提取工具调用，不依赖外层包裹完整。

## API Coverage

| Endpoint | Method | Status |
|----------|--------|--------|
| `/chat/completions` | POST | Supported (sync + SSE stream, thinking mode, tool calling, text protocol fallback) |
| `/models` | GET | Supported |
| `/user/balance` | GET | Supported |

## Changelog

### 0.1.1

- **V4 接口适配**：`reasoning_effort` 新增 `Low` 档；思考模式开启时省略 `temperature`/`top_p`；移除未文档化的 `budget_tokens`
- **文本协议兜底**：内置工具调用文本协议解析（原生 `<｜tool▁calls` + DSML `<｜｜DSML`），三态状态机（Idle → Probing → Confirmed），仅 `tools` 请求启用
- **流式健壮性**：单 chunk 多事件收集；空 delta（role 首片 / finish_reason 结束片）静默跳过；`stream_options.include_usage` 自动设置
- **兼容性**：`reasoning_content` 添加 `thinking_content` 别名；`response_format` 经 metadata 透传

### 0.1.0

- 初始发布：Chat Completions（同步 + SSE 流式）、模型列表、余额查询、上下文缓存

## License

Apache License 2.0
