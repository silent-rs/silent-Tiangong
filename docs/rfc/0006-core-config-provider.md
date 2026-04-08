# RFC 0006：CoreConfig 配置注入与热更新

> 状态：草稿
> 日期：2026-04-08
> 关联：`crates/tiangong-core/src/core/mod.rs`

## 1. 问题

当前 TiangongCore 通过内部 `build_engine()` 函数自行从磁盘加载配置：

```rust
fn build_engine() -> RuntimeEngine {
    let models_config = ModelsConfig::load();           // ~/.tiangong/models.json
    let mcp_config = read("~/.tiangong/mcp.json");      // 硬编码路径
    let skills_config = read("~/.tiangong/skills.json"); // 硬编码路径
    // ...
}
```

这带来以下问题：

1. **配置路径硬编码**：Core 假设配置文件在 `~/.tiangong/`，无法自定义
2. **与外部配置不同步**：GUI 的 TiangongState 维护一套配置（内存），Core 从磁盘读另一套，可能不一致
3. **无法热更新**：用户在 GUI 中切换模型、安装 Skill，Core 不感知，需重启对话
4. **不可嵌入**：第三方开发者无法自定义配置来源（如从数据库、远程服务加载）
5. **配置加载职责混乱**：Core 既是执行引擎又是配置加载器，违反单一职责

## 2. 目标

- TiangongCore 只定义**最小配置契约**（CoreConfig），不关心配置从哪来
- 配置通过**注入**而非自行加载，CLI/GUI/Server/第三方各自构建
- 支持**热更新**：外部修改配置后，Core 下一轮自动生效
- 第三方开发者可**零成本接入**：只需构造一个 CoreConfig 即可运行智能体

## 3. 设计

### 3.1 CoreConfig 最小契约

定义在 `tiangong-core` 中，是 TiangongCore 运行所需的全部配置：

```rust
/// TiangongCore 运行所需的最小配置
///
/// 不包含 UI 偏好、session 策略、日志级别等外围配置。
/// 只关心：用什么模型、有什么工具、什么权限。
pub struct CoreConfig {
    /// LLM 模型配置（Provider + Model + Routing）
    pub models: ModelsConfig,
    /// MCP 服务配置（server 列表）
    pub mcp: McpConfig,
    /// Skill 配置（已安装的 skill 列表）
    pub skills: SkillsConfig,
    /// 权限信任模式
    pub trust_mode: TrustMode,
    /// 上下文窗口大小（token 数）
    pub context_limit: usize,
}
```

| 字段 | 用途 | 变更频率 |
|------|------|----------|
| `models` | LLM 调用 + 多媒体能力判断（图片/语音/视频工具注入） | 低（用户切换模型） |
| `mcp` | MCP server 列表 → 注册为 function tools | 低（用户添加/移除 server） |
| `skills` | Skill 列表 → system prompt 描述 + get_skill_detail 工具 | 低（用户安装/卸载 skill） |
| `trust_mode` | 工具执行前审批策略 | 极低 |
| `context_limit` | Prompt 裁剪上限 | 极低 |

### 3.2 CoreConfigProvider

配置容器，支持无锁读和原子更新：

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use arc_swap::ArcSwap;

/// 配置提供者
///
/// 多线程安全：
/// - 读操作无锁（ArcSwap::load，纳秒级）
/// - 写操作原子替换（不阻塞读）
/// - generation 递增用于快速变更检测
pub struct CoreConfigProvider {
    inner: Arc<ArcSwap<CoreConfig>>,
    generation: Arc<AtomicU64>,
}

impl CoreConfigProvider {
    /// 创建配置提供者
    pub fn new(config: CoreConfig) -> Self { ... }

    /// 获取当前配置快照（零成本，返回 Arc 引用）
    pub fn snapshot(&self) -> Arc<CoreConfig> { ... }

    /// 获取配置版本号（原子读，用于变更检测）
    pub fn generation(&self) -> u64 { ... }

    /// 更新配置（原子替换 + generation 递增）
    pub fn update(&self, f: impl FnOnce(&mut CoreConfig)) { ... }

    /// 整体替换配置
    pub fn replace(&self, config: CoreConfig) { ... }
}

impl Clone for CoreConfigProvider {
    // 浅拷贝：多个持有者共享同一配置
    fn clone(&self) -> Self { ... }
}
```

### 3.3 TiangongCore 接口变更

```rust
impl TiangongCore {
    /// 创建新对话
    pub fn new(
        config: CoreConfigProvider,
        stream_tx: Sender<StreamEvent>,
    ) -> Self { ... }

    /// 从已有 session 创建
    pub fn with_session(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<StreamEvent>,
    ) -> Self { ... }
}
```

### 3.4 Worker 线程配置消费

```rust
fn worker_loop(
    config: CoreConfigProvider,
    session: Session,
    stream_tx: Sender<StreamEvent>,
    cmd_rx: Receiver<Command>,
) -> Session {
    let mut last_gen = 0u64;
    let mut engine: Option<RuntimeEngine> = None;
    let mut tools: Vec<FunctionToolSpec> = Vec::new();
    let mut mcp_targets: HashMap<String, McpFunctionTarget> = HashMap::new();

    loop {
        // 配置变更检测（一次原子读，纳秒级）
        let gen = config.generation();
        if engine.is_none() || gen != last_gen {
            let cfg = config.snapshot();
            engine = Some(build_engine_from_config(&cfg));
            (tools, mcp_targets) = init_tools(engine.as_ref().unwrap());
            last_gen = gen;
        }

        match cmd_rx.recv() {
            Ok(Command::Message(content)) => {
                execute_turn(&mut session, &content, engine.as_ref().unwrap(), ...);
            }
            // ...
        }
    }
}
```

## 4. 各端使用方式

### 4.1 CLI

```rust
// CLI 拥有配置的完整控制权
let config = CoreConfig::load_from_disk();  // tiangong-config 提供
let provider = CoreConfigProvider::new(config);
let core = TiangongCore::new(provider, stream_tx);

// CLI 中用户执行 /model 切换模型
provider.update(|c| {
    c.models = new_models_config;
});
// 下一轮对话自动使用新模型
```

### 4.2 GUI

```rust
// GUI 创建全局 provider，所有 Core 共享
let config = CoreConfig::load_from_disk();
let provider = CoreConfigProvider::new(config);

// 多会话共享同一 provider
let core_a = TiangongCore::with_session(provider.clone(), session_a, tx_a);
let core_b = TiangongCore::with_session(provider.clone(), session_b, tx_b);

// 用户在设置面板切换模型 → 所有会话下一轮生效
provider.update(|c| c.models = new_config);
```

### 4.3 Server

```rust
// Server 启动时加载配置
let provider = CoreConfigProvider::new(CoreConfig::load_from_disk());

// API 端点：POST /config/models
async fn update_models(provider: &CoreConfigProvider, new_config: ModelsConfig) {
    provider.update(|c| c.models = new_config);
    // 所有活跃会话下一轮自动使用新配置
}
```

### 4.4 第三方开发者（二次开发）

```rust
use tiangong_core::{CoreConfig, CoreConfigProvider, TiangongCore};

// 最简使用：手动构建配置
let config = CoreConfig {
    models: ModelsConfig::from_env(),    // 从环境变量
    mcp: McpConfig::default(),           // 无 MCP
    skills: SkillsConfig::default(),     // 无 Skill
    trust_mode: TrustMode::Full,         // 完全信任
    context_limit: 32_768,
};
let provider = CoreConfigProvider::new(config);
let (tx, rx) = std::sync::mpsc::channel();
let core = TiangongCore::new(provider, tx);

// 发送消息
core.send_message("你好".to_string());

// 消费事件流
for event in rx.iter() {
    match event {
        StreamEvent::Delta { content } => print!("{content}"),
        StreamEvent::Done { .. } => break,
        _ => {}
    }
}
```

## 5. 配置层次

```
┌────────────────────────────────────────────────────────┐
│  应用层配置（tiangong-config 或第三方实现）               │
│  ├── UI 偏好、主题、快捷键                               │
│  ├── Session 管理策略（自动保存、历史上限）               │
│  ├── 日志级别、存储路径                                  │
│  ├── Connector 配置（Telegram/Discord/...）              │
│  ├── Server 配置（端口、认证）                           │
│  └── 构建 CoreConfig ──────────────────────┐            │
└────────────────────────────────────────────│────────────┘
                                             ↓
┌────────────────────────────────────────────────────────┐
│  核心配置（CoreConfig，tiangong-core 定义）              │
│  ├── models: ModelsConfig    → LLM 调用                 │
│  ├── mcp: McpConfig          → MCP 工具注册             │
│  ├── skills: SkillsConfig    → Skill 工具注册           │
│  ├── trust_mode: TrustMode   → 权限策略                 │
│  └── context_limit: usize    → 上下文裁剪               │
└────────────────────────────────────────────────────────┘
```

**原则**：
- CoreConfig 只包含 TiangongCore 运行**必需**的配置
- 应用层配置可以任意丰富，但只向 Core 暴露 CoreConfig
- 第三方开发者只需关心 CoreConfig，无需了解应用层配置

## 6. MCP 能力缓存

MCP 工具注册依赖**能力缓存**（全局 `capability_index`）。当前由 `load_mcp_capabilities_cache` 和 `refresh_mcp_capabilities_async` 管理。

配置注入后的变更：
- `CoreConfigProvider::update` 更新 MCP 配置时，同步触发能力刷新
- 或者：将 MCP 能力缓存纳入 CoreConfig（`mcp_capabilities: Vec<(String, Vec<McpToolMeta>)>`），由外部负责填充
- 推荐后者：Core 不发起网络请求，能力数据由外部提供

```rust
pub struct CoreConfig {
    pub models: ModelsConfig,
    pub mcp: McpConfig,
    pub mcp_capabilities: Vec<(String, Vec<McpToolMeta>)>,  // 预填充的能力数据
    pub skills: SkillsConfig,
    pub trust_mode: TrustMode,
    pub context_limit: usize,
}
```

这样 Core 完全不依赖磁盘缓存和网络刷新，所有数据由外部注入。

## 7. 实施计划

### Phase A：CoreConfig + CoreConfigProvider
1. 在 `tiangong-core` 中定义 `CoreConfig` 和 `CoreConfigProvider`
2. 修改 `TiangongCore` 构造函数接收 `CoreConfigProvider`
3. 修改 `worker_loop` 使用 provider 的 generation 检测 + snapshot
4. 移除 `build_engine()` 中的磁盘加载逻辑

### Phase B：各端适配
1. CLI：创建 `CoreConfigProvider`，从磁盘加载初始配置
2. GUI：`TiangongApp` 持有 `CoreConfigProvider`，配置变更时调用 `update`
3. Server：启动时加载，API 端点触发 `update`

### Phase C：tiangong-config 独立 crate（可选）
1. 提取磁盘加载、持久化、文件监听为独立 crate
2. 提供 `CoreConfig::load_from_disk()` 便捷方法
3. CLI/GUI/Server 依赖 `tiangong-config`，第三方开发者可选

## 8. 二次开发指南

### 8.1 最小集成

只需 `tiangong-core` 一个依赖：

```toml
[dependencies]
tiangong-core = { path = "crates/tiangong-core" }
tiangong-types = { path = "crates/tiangong-types" }
```

```rust
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
use tiangong_types::StreamEvent;

fn main() {
    let config = CoreConfig::builder()
        .with_model("openai", "https://api.openai.com/v1", "sk-xxx", "gpt-4o")
        .build();

    let provider = CoreConfigProvider::new(config);
    let (tx, rx) = std::sync::mpsc::channel();
    let core = TiangongCore::new(provider, tx);

    core.send_message("Hello".into());
    for event in rx.iter() {
        match event {
            StreamEvent::Delta { content } => print!("{content}"),
            StreamEvent::Done { .. } => break,
            _ => {}
        }
    }
}
```

### 8.2 添加 MCP 工具

```rust
use tiangong_core::agent_config::{McpConfig, McpServerConfig};

let config = CoreConfig::builder()
    .with_model("openai", "https://api.openai.com/v1", "sk-xxx", "gpt-4o")
    .with_mcp_server(McpServerConfig {
        name: "my-tools".into(),
        command: "npx".into(),
        args: vec!["my-mcp-server".into()],
        enabled: true,
        ..Default::default()
    })
    .build();
```

### 8.3 动态更新配置

```rust
let provider = CoreConfigProvider::new(initial_config);
let core = TiangongCore::new(provider.clone(), tx);

// 运行时切换模型（下一轮对话自动生效）
provider.update(|c| {
    c.models = ModelsConfig::from_single_provider(
        "deepseek",
        "https://api.deepseek.com/v1",
        "sk-xxx",
        "deepseek-chat",
    );
});

// 运行时添加 MCP server（下一轮对话自动注册工具）
provider.update(|c| {
    c.mcp.servers.push(McpServerConfig { ... });
    c.mcp_capabilities.push(("server-name".into(), tools));
});
```

### 8.4 自定义配置来源

```rust
// 从数据库加载
let config = load_config_from_database(&db)?;
let provider = CoreConfigProvider::new(config);

// 从远程配置中心加载
let config = fetch_config_from_remote("https://config.example.com/agent")?;
let provider = CoreConfigProvider::new(config);

// 监听配置变更
std::thread::spawn(move || {
    loop {
        let new_config = fetch_config_from_remote("...").unwrap();
        provider.replace(new_config);
        std::thread::sleep(Duration::from_secs(60));
    }
});
```

### 8.5 嵌入到现有系统

```rust
// 作为 HTTP 服务的后端
async fn chat_handler(req: ChatRequest, provider: &CoreConfigProvider) -> ChatResponse {
    let (tx, rx) = mpsc::channel();
    let session = load_session(&req.session_id);
    let core = TiangongCore::with_session(provider.clone(), session, tx);
    core.send_message(req.message);

    let mut response = String::new();
    for event in rx.iter() {
        if let StreamEvent::Delta { content } = event {
            response.push_str(&content);
        }
        if matches!(event, StreamEvent::Done { .. }) {
            break;
        }
    }
    ChatResponse { content: response }
}
```

## 9. 非目标

- 不在 CoreConfig 中包含 UI/Server/Connector 配置
- 不在 TiangongCore 中实现配置持久化（由外部负责）
- 不在 TiangongCore 中发起网络请求加载配置（由外部注入）
- 不改变 StreamEvent 输出协议
