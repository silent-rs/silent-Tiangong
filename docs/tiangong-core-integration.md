# TiangongCore 对接文档

## 文档目标

本文档面向以下接入方：

- CLI / GUI / Server 宿主层
- Connector / Gateway 调用方
- 第三方 Rust 项目嵌入方

目标是说明如何以最小成本接入 `TiangongCore`，并避免直接依赖 GUI/Server 侧的实现细节。

## 核心定位

`TiangongCore` 是天工的统一执行核心，负责完成以下链路：

- 接收用户输入
- 构建 Prompt
- 调用模型
- 执行工具与 MCP
- 更新会话
- 输出流式事件

代码入口见：

- [core/mod.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-core/src/core/mod.rs)
- [core_config.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-core/src/core_config.rs)

`TiangongCore` 不负责：

- 加载磁盘配置
- 管理 GUI 状态
- 管理 HTTP 生命周期
- 管理 Connector 凭据
- 持久化应用层配置

这些职责应由宿主层处理，再通过 `CoreConfigProvider` 注入给 Core。

## 对接总览

最小接入链路如下：

1. 构造 `CoreConfig`
2. 用 `CoreConfigProvider` 包装配置
3. 创建 `std::sync::mpsc` 通道接收 `SessionStreamEvent`
4. 创建 `TiangongCore`
5. 调用 `send_message`
6. 消费事件流并驱动你的宿主 UI / API / Bot 输出
7. 结束时调用 `into_session` 取回最终会话

## 关键类型

### `TiangongCore`

主要接口位于 [core/mod.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-core/src/core/mod.rs)：

```rust
pub struct TiangongCore;

impl TiangongCore {
    pub fn new(
        config: CoreConfigProvider,
        stream_tx: Sender<SessionStreamEvent>,
    ) -> Self;

    pub fn with_session(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
    ) -> Self;

    pub fn send_message(&self, content: String);
    pub fn cancel(&self);
    pub fn respond_approval(&self, request_id: String, approved: bool);
    pub fn set_trust_mode(&self, mode: TrustMode);
    pub fn session_id(&self) -> &str;
    pub fn into_session(self) -> Session;
}
```

### `CoreConfig`

定义位于 [core_config.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-core/src/core_config.rs)。

当前运行所需最小配置包括：

- `llm`: 模型端点配置
- `mcp`: MCP server 配置
- `mcp_capabilities`: 预填充 MCP 能力数据
- `skills`: 已安装技能配置
- `trust_mode`: 工具审批策略
- `context_limit`: 上下文窗口上限

### `CoreConfigProvider`

`CoreConfigProvider` 是线程安全的配置容器，提供：

- `snapshot()`: 读取当前配置快照
- `generation()`: 获取配置版本
- `update(...)`: 原子更新配置
- `replace(...)`: 整体替换配置

宿主层不应让 Core 自己去读磁盘配置，而应该自己构造并注入。

### `SessionStreamEvent`

事件类型来自 `tiangong-types`，宿主层主要消费：

- `UserMessage`
- `Reasoning`
- `Delta`
- `ToolCalls`
- `ToolStart`
- `ToolResult`
- `ApprovalNeeded`
- `Done`
- `Error`

如果你的宿主支持多会话，务必使用 `SessionStreamEvent.session_id` 做路由，而不是假设单会话。

## 最小可运行示例

仓库内现成示例见：

- [test_core.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-core/examples/test_core.rs)

一个最小接入示例如下：

```rust
use std::sync::mpsc;

use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
use tiangong_types::{SessionStreamEvent, StreamEvent};

fn main() {
    let provider = CoreConfigProvider::new(CoreConfig::default());
    let (tx, rx) = mpsc::channel::<SessionStreamEvent>();

    let core = TiangongCore::new(provider, tx);
    core.send_message("你好".to_string());

    loop {
        match rx.recv() {
            Ok(se) => match se.event {
                StreamEvent::Delta { content, .. } => print!("{content}"),
                StreamEvent::Reasoning { content, .. } => eprint!("{content}"),
                StreamEvent::Done { .. } => break,
                StreamEvent::Error { message } => {
                    eprintln!("error: {message}");
                    break;
                }
                _ => {}
            },
            Err(_) => break,
        }
    }

    let session = core.into_session();
    println!("session_id={}", session.id);
}
```

## 推荐初始化方式

### 方式一：直接构造 `CoreConfig`

适合第三方嵌入或测试环境。

```rust
use tiangong_core::core_config::{CoreConfig, CoreConfigProvider, LlmConfig, ModelEndpoint};

let provider = CoreConfigProvider::new(CoreConfig {
    llm: LlmConfig {
        chat: ModelEndpoint {
            base_url: std::env::var("API_BASE_URL").unwrap_or_default(),
            api_key: std::env::var("API_AUTH_TOKEN").unwrap_or_default(),
            model: std::env::var("API_MODEL").unwrap_or_else(|_| "gpt-4.1".to_string()),
            timeout_ms: 120_000,
        },
        lite: None,
        image_generation: None,
        tts: None,
        stt: None,
        video_generation: None,
    },
    ..CoreConfig::default()
});
```

### 方式二：通过 `tiangong-config` 注入

适合 CLI / GUI / Server 正式宿主。

相关代码：

- [crates/tiangong-config/src/lib.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-config/src/lib.rs)
- [crates/tiangong-config/src/config.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-config/src/config.rs)

示例：

```rust
use tiangong_config::load_tiangong_config;

let provider = load_tiangong_config().into_core_config_provider();
```

这个路径适合需要沿用现有 `models` / `mcp` / `skills` / `trust_mode` 配置体系的宿主。

## 事件流消费建议

### 推荐映射

- `Reasoning`: 显示思考/解释文本
- `Delta`: 显示最终回复增量
- `ToolCalls`: 显示本轮计划调用了哪些工具
- `ToolStart`: 显示某个工具开始执行
- `ToolResult`: 展示工具结果摘要
- `ApprovalNeeded`: 弹出审批 UI 或进入等待状态
- `Done`: 标记本轮完成
- `Error`: 标记本轮失败

### 注意事项

- `Reasoning` 和 `Delta` 都可能是流式分片，宿主层应做追加而不是覆盖。
- `Done` 表示当前轮次结束，不代表 `TiangongCore` 生命周期结束。
- `TiangongCore` 可多轮复用，同一个实例可持续 `send_message(...)`。
- 如果宿主层支持取消，应将用户操作映射到 `core.cancel()`。

## 审批与权限

当工具执行需要确认时，Core 会输出 `ApprovalNeeded`。

宿主层应：

1. 保存 `request_id`
2. 展示工具名和参数摘要
3. 用户确认后调用：

```rust
core.respond_approval(request_id, true);
```

或拒绝：

```rust
core.respond_approval(request_id, false);
```

如果宿主层需要动态切换权限模式，可直接调用：

```rust
core.set_trust_mode(mode);
```

## 会话管理

### 新会话

使用 `TiangongCore::new(...)`，Core 内部会自动创建标题为“新对话”的会话。

### 恢复已有会话

如果你已经从磁盘或数据库恢复了 `Session`，应使用：

```rust
let core = TiangongCore::with_session(provider, session, tx);
```

这适合：

- GUI 打开历史会话
- Server 从持久化记录恢复上下文
- Connector 基于会话 ID 续聊

### 收尾与持久化

当宿主层准备销毁 Core 时，应调用：

```rust
let session = core.into_session();
```

再由宿主层自行持久化 `session`。

不要依赖 `drop` 做业务层持久化，因为 `drop` 只负责发送关闭命令，不负责你的外层存储逻辑。

## 配置热更新

`CoreConfigProvider` 支持热更新。宿主层可以在不重建 Core 的情况下更新配置：

```rust
provider.update(|config| {
    config.context_limit = 64_000;
});
```

或者：

```rust
provider.replace(new_config);
```

当前行为约定：

- Core 在 worker 循环中检测 `generation`
- 当配置版本变化时，下一轮消息开始前重建 `RuntimeEngine`
- 已经开始执行的当前轮次不会被中途替换

因此，热更新语义是“下一轮生效”，不是“当前轮次中途热切换”。

## 多会话接入建议

如果宿主层要管理多个会话：

- 每个会话维护一个独立 `TiangongCore`
- 多个 Core 可以共享一个 `CoreConfigProvider`
- 事件消费层使用 `session_id` 做分发

推荐结构：

```rust
HashMap<String, TiangongCore>
```

共享配置适合：

- GUI 多标签会话
- Server 多客户端会话
- Connector 多用户会话

## 与宿主层的职责边界

宿主层应负责：

- 配置加载与保存
- 会话列表管理
- UI 状态管理
- 审批交互
- 日志策略
- 网络 API
- 持久化与恢复

Core 应负责：

- 对话轮执行
- Prompt 装配
- 工具调度
- StreamEvent 输出
- 会话内消息更新

如果宿主层把 UI 状态、配置存储、事件执行都混进 Core，会重新引入现在 RFC 0006 想解决的问题。

## 常见接入错误

### 1. 让 Core 自己读磁盘配置

不推荐。应由宿主读取后注入 `CoreConfigProvider`。

### 2. 只处理 `Delta`，忽略 `Reasoning`

这样会丢失“边执行边解释”的体验，也会影响调试。

### 3. 忽略 `ApprovalNeeded`

如果你的权限模式不是全信任，这会导致执行卡住。

### 4. 直接 `drop(core)` 而不取回 `Session`

这样宿主层可能拿不到最终会话快照，无法正确持久化。

### 5. 配置更新后立即假设当前轮次生效

当前实现是“下一轮生效”。

## 参考材料

- [docs/rfc/0006-core-config-provider.md](/Users/hubertshelley/Documents/silent/tiangong/docs/rfc/0006-core-config-provider.md)
- [crates/tiangong-core/examples/test_core.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-core/examples/test_core.rs)
- [crates/tiangong-core/src/core/mod.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-core/src/core/mod.rs)
- [crates/tiangong-core/src/core_config.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-core/src/core_config.rs)
- [crates/tiangong-config/src/lib.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-config/src/lib.rs)
