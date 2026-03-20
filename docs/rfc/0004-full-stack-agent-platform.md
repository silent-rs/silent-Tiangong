# RFC 0004: 全栈个人智能终端平台

> 状态：草案
> 创建：2026-03-20
> 作者：hubertshelley

## 动机

天工当前是一个桌面级 AI 自动化中枢，具备 CLI/TUI 和桌面 GUI 两种交互模式。但现有架构存在以下局限：

1. **交互入口单一**：仅支持本地桌面/终端交互，无法远程调度
2. **能力边界窄**：仅支持文本对话与工具执行，缺少图片/视频等多媒体生成能力
3. **部署模式单一**：仅支持单机桌面运行，无法作为服务部署
4. **缺乏通道接入**：无法通过 IM 软件（微信、Telegram、Discord 等）远程触发

参考 OpenClaw 的 Hub-and-Spoke 架构理念，将天工重构为一个**全功能可扩展的 GUI + CLI + Server 个人智能终端**，通过 Connector 机制接入各类 IM 通道实现远程调度。

## 目标

### 核心目标
- 保留现有 GUI（Tauri + React）和 CLI/TUI 交互能力
- 新增 Server 模式，支持 HTTP API / WebSocket 服务部署
- 新增 Connector 层，支持通过 IM 软件远程调度 Agent
- 新增多媒体生成能力（图片生成、视频生成）
- 架构插件化，所有能力均可按需装配

### 非目标（首期）
- 多用户权限与租户隔离
- 商业化计费
- 公共 Skill 市场

## 架构设计

### 总体架构（6 层）

```
┌─────────────────────────────────────────────────────────────┐
│  6. 前端层 (Frontends)                                      │
│     ├─ GUI (Tauri + React + shadcn/ui)                      │
│     ├─ CLI/TUI (ratatui + crossterm)                        │
│     └─ Server API (HTTP REST + WebSocket)                   │
└─────────────────────────────────────────────────────────────┘
                            ↕
┌─────────────────────────────────────────────────────────────┐
│  5. 网关层 (Gateway)                                        │
│     ├─ 消息路由与会话管理                                    │
│     ├─ Connector 适配器管理                                  │
│     ├─ 认证与访问控制                                        │
│     └─ 速率限制与消息队列                                    │
└─────────────────────────────────────────────────────────────┘
                            ↕
┌─────────────────────────────────────────────────────────────┐
│  4. 智能体运行时层 (Agent Runtime)                           │
│     ├─ RuntimeEngine (装配 planning → execution → response)  │
│     ├─ 多 Agent 路由与隔离                                   │
│     └─ 会话上下文管理                                        │
└─────────────────────────────────────────────────────────────┘
                            ↕
┌─────────────────────────────────────────────────────────────┐
│  3. 能力层 (Capabilities)                                    │
│     ├─ Tool (本地工具：文件、命令、搜索)                      │
│     ├─ MCP (Model Context Protocol 客户端)                   │
│     ├─ Skill (Skill 生命周期管理)                            │
│     ├─ MediaGen (图片/视频生成)                              │
│     └─ Connector (IM 通道适配器)                             │
└─────────────────────────────────────────────────────────────┘
                            ↕
┌─────────────────────────────────────────────────────────────┐
│  2. 智能体层 (Agents)                                        │
│     ├─ PlanningAgent (规划)                                  │
│     ├─ ExecutionAgent (执行)                                 │
│     ├─ ResponseAgent (响应)                                  │
│     ├─ MediaAgent (多媒体生成调度)                           │
│     └─ RoutingAgent (多通道路由决策)                          │
└─────────────────────────────────────────────────────────────┘
                            ↕
┌─────────────────────────────────────────────────────────────┐
│  1. 基础层 (Foundation)                                      │
│     ├─ AppState (配置/状态/持久化)                           │
│     ├─ Model (多模型客户端抽象)                              │
│     ├─ Session (会话管理)                                    │
│     └─ EventBus (事件总线)                                   │
└─────────────────────────────────────────────────────────────┘
```

### Workspace 拆分

从单 crate 重构为 Cargo workspace，实现关注点分离：

```
tiangong/
├── Cargo.toml                    # workspace 根
├── crates/
│   ├── tiangong-core/            # 核心引擎（无 UI 依赖）
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── app_state/        # 配置与状态层
│   │       ├── agents/           # 智能体层
│   │       ├── execution/        # 执行器层
│   │       ├── model/            # 模型客户端
│   │       ├── session/          # 会话管理
│   │       ├── tool/             # 本地工具
│   │       ├── mcp/              # MCP 客户端
│   │       ├── skill/            # Skill 管理
│   │       └── event/            # 事件总线
│   │
│   ├── tiangong-media/           # 多媒体生成能力
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── image/            # 图片生成（DALL-E / SD / Flux 等）
│   │       └── video/            # 视频生成（Sora / Runway / Kling 等）
│   │
│   ├── tiangong-gateway/         # 网关层
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── router.rs         # 消息路由
│   │       ├── auth.rs           # 认证与访问控制
│   │       └── queue.rs          # 消息队列
│   │
│   ├── tiangong-server/          # Server 模式（HTTP/WS 服务）
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── api/              # REST API 端点
│   │       └── ws/               # WebSocket 端点
│   │
│   ├── tiangong-connector/       # Connector 框架与内置适配器
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # Connector trait 定义
│   │       ├── telegram.rs       # Telegram Bot 适配器
│   │       ├── discord.rs        # Discord Bot 适配器
│   │       ├── feishu.rs         # 飞书/Lark 适配器
│   │       ├── dingtalk.rs       # 钉钉适配器
│   │       ├── slack.rs          # Slack 适配器
│   │       └── webhook.rs        # 通用 Webhook 适配器
│   │
│   ├── tiangong-cli/             # CLI/TUI 前端
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── tui/              # TUI 组件
│   │
│   └── tiangong-gui/             # 桌面 GUI 前端（Tauri + React）
│       ├── Cargo.toml
│       ├── src/                  # Tauri 后端
│       └── frontend/             # React 前端
│           ├── package.json
│           └── src/
│
├── src/                          # 主二进制入口
│   └── main.rs                   # 统一入口分发
│
├── configs/                      # 默认配置模板
└── docs/                         # 文档
```

### 运行模式

天工支持 3 种运行模式，通过命令行参数切换：

```bash
# 1. GUI 模式（默认）— 桌面应用
tiangong

# 2. CLI/TUI 模式 — 终端交互
tiangong cli

# 3. Server 模式 — HTTP API + WebSocket + Connector 一体化服务
tiangong server [--port 8080] [--host 0.0.0.0]

# Server 后台运行（-d / --daemon）
tiangong server -d [--port 8080] [--host 0.0.0.0]
tiangong server --daemon [--port 8080] [--host 0.0.0.0]

# 停止后台运行的 Server
tiangong server stop
```

Server 模式启动时自动加载并启动所有已启用的 Connector。通过 `-d` / `--daemon` 参数可在后台运行（fork 进程后主进程退出，PID 写入 `~/.tiangong/server.pid`），适合部署在服务器上。`tiangong server stop` 读取 PID 文件并发送信号停止后台进程。

### Connector 机制

Connector 是天工接入外部 IM 通道的标准化适配层，类似 OpenClaw 的 Channel Adapter。

#### Connector Trait 定义

```rust
#[async_trait]
pub trait Connector: Send + Sync {
    /// 连接器名称
    fn name(&self) -> &str;

    /// 启动连接器，开始监听消息
    async fn start(&mut self, sender: MessageSender) -> Result<()>;

    /// 停止连接器
    async fn stop(&mut self) -> Result<()>;

    /// 发送消息到通道
    async fn send_message(&self, channel_id: &str, message: OutgoingMessage) -> Result<()>;

    /// 发送媒体文件到通道
    async fn send_media(&self, channel_id: &str, media: MediaPayload) -> Result<()>;

    /// 连接器健康检查
    async fn health_check(&self) -> Result<ConnectorStatus>;
}
```

#### 统一消息模型

```rust
/// 入站消息（从 IM 到天工）
pub struct IncomingMessage {
    pub id: String,               // scru128
    pub connector: String,        // 来源连接器名称
    pub channel_id: String,       // 通道标识
    pub sender_id: String,        // 发送者标识
    pub content: MessageContent,  // 消息内容（文本/图片/文件/语音）
    pub reply_to: Option<String>, // 回复消息 ID
    pub timestamp: NaiveDateTime,
}

/// 出站消息（从天工到 IM）
pub struct OutgoingMessage {
    pub content: MessageContent,
    pub reply_to: Option<String>,
    pub attachments: Vec<MediaPayload>,
}

/// 消息内容
pub enum MessageContent {
    Text(String),
    Image { url: String, caption: Option<String> },
    File { url: String, name: String },
    Audio { url: String, duration: Option<u32> },
    Video { url: String, caption: Option<String> },
    Mixed(Vec<MessageContent>),
}

/// 媒体载荷
pub struct MediaPayload {
    pub media_type: MediaType,
    pub data: Vec<u8>,          // 二进制数据
    pub filename: Option<String>,
    pub mime_type: String,
}
```

### Server API 设计

Server 模式暴露 HTTP REST + WebSocket 两类接口：

#### REST API

```
POST   /api/v1/chat                    # 发送消息（同步/流式）
POST   /api/v1/chat/stream             # 流式对话（SSE）
GET    /api/v1/sessions                 # 会话列表
POST   /api/v1/sessions                 # 创建会话
GET    /api/v1/sessions/:id             # 会话详情
DELETE /api/v1/sessions/:id             # 删除会话

POST   /api/v1/media/image/generate     # 图片生成
POST   /api/v1/media/video/generate     # 视频生成
GET    /api/v1/media/tasks/:id          # 查询生成任务状态

GET    /api/v1/skills                   # Skill 列表
POST   /api/v1/skills/install           # 安装 Skill
DELETE /api/v1/skills/:id               # 卸载 Skill

GET    /api/v1/mcp                      # MCP 列表
POST   /api/v1/mcp                      # 添加 MCP
DELETE /api/v1/mcp/:name                # 删除 MCP

GET    /api/v1/connectors               # Connector 列表
POST   /api/v1/connectors/:name/start   # 启动 Connector
POST   /api/v1/connectors/:name/stop    # 停止 Connector
GET    /api/v1/connectors/:name/status  # Connector 状态

GET    /api/v1/health                   # 健康检查
POST   /api/v1/server/shutdown           # 优雅关闭 Server
```

#### WebSocket

```
WS /api/v1/ws    # 双向实时通信（对话流、事件推送、Connector 状态）
```

### 多媒体生成能力

#### 图片生成

通过统一的 `ImageGenerator` trait 支持多后端：

```rust
#[async_trait]
pub trait ImageGenerator: Send + Sync {
    fn name(&self) -> &str;
    async fn generate(&self, request: ImageGenRequest) -> Result<ImageGenResponse>;
    async fn edit(&self, request: ImageEditRequest) -> Result<ImageGenResponse>;
}

pub struct ImageGenRequest {
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub width: u32,
    pub height: u32,
    pub model: Option<String>,       // 指定模型
    pub style: Option<String>,       // 风格
    pub num_images: u32,             // 生成数量
}
```

计划支持的后端：
- OpenAI DALL-E 3 / GPT-Image
- Stable Diffusion (本地/远程 API)
- Flux
- Midjourney API（第三方）

#### 视频生成

```rust
#[async_trait]
pub trait VideoGenerator: Send + Sync {
    fn name(&self) -> &str;
    async fn generate(&self, request: VideoGenRequest) -> Result<VideoGenTask>;
    async fn query_status(&self, task_id: &str) -> Result<VideoGenStatus>;
}

pub struct VideoGenRequest {
    pub prompt: String,
    pub duration: Option<u32>,       // 时长（秒）
    pub resolution: Option<String>,  // 分辨率
    pub model: Option<String>,
    pub reference_image: Option<Vec<u8>>, // 参考图
}
```

计划支持的后端：
- Sora
- Runway Gen
- Kling（可灵）
- Pika

### 事件总线

引入轻量事件总线实现各层解耦通信：

```rust
pub enum TiangongEvent {
    // 会话事件
    MessageReceived(IncomingMessage),
    MessageSent(OutgoingMessage),
    SessionCreated(String),

    // Agent 事件
    PlanCreated(TaskPlan),
    StepStarted { plan_id: String, step_index: usize },
    StepCompleted { plan_id: String, step_index: usize, result: StepResult },
    TurnCompleted(TurnResult),

    // 媒体事件
    MediaTaskCreated(String),
    MediaTaskCompleted { task_id: String, result: MediaResult },

    // Connector 事件
    ConnectorStarted(String),
    ConnectorStopped(String),
    ConnectorError { name: String, error: String },

    // 系统事件
    ConfigChanged,
    HealthCheck,
}
```

### 配置结构扩展

```
~/.tiangong/
├── app.json                # 应用主配置（会话/UI 状态）
├── models.json             # 模型配置（常规/多模态/图片生成/视频生成）
├── server.json             # Server 模式配置（端口/认证/CORS）
├── server.pid              # Server 后台运行时的 PID 文件（自动生成）
├── connectors.json         # Connector 配置（各通道凭据与开关）
├── mcp.json                # MCP 配置
├── skills.json             # Skill 配置
├── mcp-lock.json           # MCP 锁文件
├── skills-lock.json        # Skill 锁文件
├── mcp-tools-cache.json    # MCP 能力缓存
├── sessions/               # 会话持久化
├── skills/                 # Skill 存储
├── media/                  # 生成的媒体文件缓存
│   ├── images/
│   └── videos/
└── logs/                   # 日志目录
```

#### models.json 示例

模型配置采用 **Provider 与 Model 分离** 设计：Provider 只定义连接信息（可被多个模型共享），Model 引用 Provider 并声明自身能力，Routing 表按用途指定默认模型。

```json
{
  "providers": {
    "openai": {
      "base_url": "https://api.openai.com/v1",
      "api_key": "${OPENAI_API_KEY}",
      "timeout_ms": 60000
    },
    "deepseek": {
      "base_url": "https://api.deepseek.com/v1",
      "api_key": "${DEEPSEEK_API_KEY}",
      "timeout_ms": 60000
    },
    "kling": {
      "base_url": "https://api.klingai.com/v1",
      "api_key": "${KLING_API_KEY}",
      "timeout_ms": 300000
    }
  },

  "models": {
    "gpt-4o": {
      "provider": "openai",
      "model": "gpt-4o",
      "capabilities": ["chat", "multimodal"],
      "options": { "stream": true, "max_tokens": null }
    },
    "deepseek-chat": {
      "provider": "deepseek",
      "model": "deepseek-chat",
      "capabilities": ["chat"],
      "options": { "stream": true, "max_tokens": null }
    },
    "dall-e-3": {
      "provider": "openai",
      "model": "dall-e-3",
      "capabilities": ["image_generation"],
      "options": { "size": "1024x1024", "quality": "standard", "num": 1 }
    },
    "gpt-image-1": {
      "provider": "openai",
      "model": "gpt-image-1",
      "capabilities": ["image_generation"],
      "options": { "size": "1024x1024", "quality": "auto", "num": 1 }
    },
    "kling-v1": {
      "provider": "kling",
      "model": "kling-v1",
      "capabilities": ["video_generation"],
      "options": { "duration": 5, "resolution": "1080p" }
    }
  },

  "routing": {
    "chat": "gpt-4o",
    "multimodal": "gpt-4o",
    "image_generation": "dall-e-3",
    "video_generation": "kling-v1"
  }
}
```

**设计要点**：
- **Provider 复用**：同一个 OpenAI provider 被 gpt-4o、dall-e-3、gpt-image-1 共享，连接信息只写一次
- **能力声明**：模型通过 `capabilities` 声明支持的用途，一个模型可服务多个用途（如 gpt-4o 同时支持 chat 和 multimodal）
- **按用途路由**：`routing` 表为每种用途指定默认模型，简洁明了
- **专属参数**：`options` 为 JSON 对象，各类模型携带各自专属参数，不混杂
- **环境变量**：`api_key` 支持 `${ENV_VAR}` 语法引用环境变量，避免明文存储
- **易扩展**：新增能力类型（如 `audio_generation`）只需添加模型条目 + routing 条目，无需修改结构

**模型配置数据结构**：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsConfig {
    /// Provider 连接配置（name -> config）
    pub providers: HashMap<String, ProviderConfig>,
    /// 模型定义（name -> config）
    pub models: HashMap<String, ModelConfig>,
    /// 按用途路由（capability -> model name）
    pub routing: HashMap<ModelCapability, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    /// 支持 ${ENV_VAR} 环境变量引用
    pub api_key: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// 引用 providers 中的 key
    pub provider: String,
    /// 实际模型 ID（传给 API 的 model 参数）
    pub model: String,
    /// 模型支持的能力列表
    pub capabilities: Vec<ModelCapability>,
    /// 模型专属参数（chat: stream/max_tokens, image: size/quality/num, video: duration/resolution）
    #[serde(default)]
    pub options: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    /// 常规对话/推理
    Chat,
    /// 多模态理解（支持图片/文件输入）
    Multimodal,
    /// 图片生成
    ImageGeneration,
    /// 视频生成
    VideoGeneration,
}
```

#### connectors.json 示例

```json
{
  "telegram": {
    "enabled": true,
    "bot_token": "xxx",
    "allowed_users": ["user_id_1"],
    "webhook_url": null
  },
  "discord": {
    "enabled": false,
    "bot_token": "xxx",
    "allowed_guilds": [],
    "allowed_users": []
  },
  "feishu": {
    "enabled": false,
    "app_id": "xxx",
    "app_secret": "xxx",
    "encrypt_key": null,
    "verification_token": null,
    "allowed_users": []
  },
  "webhook": {
    "enabled": true,
    "secret": "xxx",
    "endpoints": []
  }
}
```

## 实施计划

### Phase A：Workspace 拆分与核心抽离（基础重构）

**目标**：将单 crate 拆分为 workspace，核心引擎独立化

1. 创建 workspace 结构，迁移 `src/core` → `crates/tiangong-core`
2. 迁移 CLI/TUI → `crates/tiangong-cli`
3. 迁移 GUI → `crates/tiangong-gui`
4. 主二进制统一入口分发
5. 确保现有 GUI + CLI 功能不回退

### Phase B：Server 模式

**目标**：新增 HTTP API / WebSocket 服务模式

1. 新建 `crates/tiangong-server`
2. 实现 REST API（对话、会话管理）
3. 实现 WebSocket 流式通信
4. 认证与访问控制（API Token）
5. 健康检查与基础监控

### Phase C：Gateway 与事件总线

**目标**：建立统一的消息路由与事件机制

1. 实现 EventBus（基于 tokio broadcast）
2. 实现 Gateway 消息路由
3. 统一消息模型（IncomingMessage / OutgoingMessage）
4. 会话与通道映射管理

### Phase D：Connector 框架与首批适配器

**目标**：实现 Connector 机制并接入首批 IM 通道

1. 定义 Connector trait
2. 实现 Telegram Bot Connector（首个适配器）
3. 实现 Discord Bot Connector
4. 实现飞书/Lark Bot Connector
5. 实现通用 Webhook Connector
6. Connector 配置管理与热插拔
7. 后续可扩展：钉钉/Slack 等

### Phase E：多媒体生成能力

**目标**：集成图片和视频生成能力

1. 新建 `crates/tiangong-media`
2. 定义 ImageGenerator / VideoGenerator trait
3. 接入 OpenAI DALL-E / GPT-Image 图片生成
4. 接入视频生成后端（Sora/Kling）
5. 媒体任务管理与状态追踪
6. Agent 层集成 MediaAgent，支持对话中触发生成

### Phase F：生产化与完善

**目标**：整合所有能力为生产可用状态

1. 完善日志与监控
2. 配置热重载
3. 安全加固（TLS、Connector 鉴权、速率限制）
4. 部署文档与 Docker 支持

## 技术选型

| 领域 | 选型 | 理由 |
|------|------|------|
| HTTP Server | `silent` (Rust Web 框架) | 用户常用框架，轻量高效 |
| WebSocket | `silent` 内置 WS 支持 | 与 HTTP 框架统一 |
| Telegram SDK | `teloxide` | Rust 生态成熟 Telegram Bot 库 |
| Discord SDK | `serenity` | Rust 生态成熟 Discord Bot 库 |
| 飞书/Lark SDK | HTTP API 直接对接 | 飞书开放平台 REST API，无需额外 SDK |
| 图片生成 | `async-openai` (已有) | 复用现有依赖 |
| 事件总线 | `tokio::sync::broadcast` | 轻量，无额外依赖 |
| 序列化 | `serde` / `serde_json` (已有) | 复用 |
| 异步运行时 | `tokio` (已有) | 复用 |

## 与现有架构的兼容性

### 保留
- 核心 5 层架构（仅重新组织为独立 crate）
- 现有 Agent 推理链路（planning → execution → response）
- MCP 与 Skill 管理体系
- 会话模型与持久化
- GUI（Tauri + React）和 CLI/TUI 交互

### 新增
- Gateway 层（消息路由）
- Connector 层（IM 适配）
- Server 模式（HTTP/WS API）
- 多媒体生成能力
- 事件总线
- Server `-d` 后台运行

### 重构
- 单 crate → workspace 多 crate
- 事件驱动解耦（替代部分直接调用）
- 配置结构扩展（新增 server/connector/media 配置）

## 风险与缓解

| 风险 | 缓解措施 |
|------|----------|
| 重构范围大，影响现有功能 | Phase A 确保功能不回退后再进入后续阶段 |
| IM 平台 API 变更频繁 | Connector trait 抽象隔离，单适配器变更不影响全局 |
| 视频生成耗时长 | 异步任务模型，支持状态查询 |
| 飞书 API 文档复杂 | 优先实现 Bot 消息收发，高级能力（审批/日历）后续扩展 |
| Server 暴露安全风险 | 默认 127.0.0.1 绑定，API Token 认证，速率限制 |
