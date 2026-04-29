# RFC 0008：Prompt 缓存优化——消除多层缓存破坏点

> 状态：草稿
> 日期：2026-04-28
> 关联：`crates/tiangong-anthropic`、`crates/tiangong-llm`、`crates/tiangong-core/src/prompt/`、`crates/tiangong-core/src/core/mod.rs`

---

## 1. 问题

当前系统在与 Anthropic 交互时缓存命中率极低，实测第二轮问答的 prompt token 几乎没有被缓存复用。

根因不是单一问题，而是贯穿三个层级的系统性缺陷：**类型层不支持显式缓存标注**、**映射层将所有内容压缩成单一字符串**、**Core 层存在多个高波动注入点**。这三层问题叠加，导致每轮请求的 prompt 都在变化，Anthropic 的隐式前缀缓存无法命中，显式 Prompt Caching 也完全缺失。

---

## 2. 背景：Anthropic Prompt Caching 工作原理

Anthropic 提供两种缓存机制：

### 2.1 隐式前缀缓存（Implicit Prefix Caching，claude-3.7+ 默认开启）

- 系统自动对超过 **1024 tokens** 的相同前缀进行缓存
- 无需任何标注，只要请求前缀完全相同即可命中
- 但只要前缀的任意位置发生变化（哪怕是 system 末尾追加了一行），缓存即失效
- 缓存 TTL 约 5 分钟

### 2.2 显式 Prompt Caching（需要 `anthropic-beta: prompt-caching-2024-07-31` Header）

- 在内容块上标注 `cache_control: {"type": "ephemeral"}`，明确指定缓存断点
- 最多支持 4 个缓存断点
- 缓存 TTL 为 5 分钟
- System blocks、消息内容块、Tool 定义均可标注
- 最佳实践：在稳定内容的最后一个块上打断点，不稳定内容不打断点

**关键认知**：显式缓存的 `cache_control` 是指"在此处建立缓存检查点"，之后的内容不影响此检查点命中。因此，正确的做法是：
- 在静态规则块末尾打断点 → 跨会话命中
- 在历史消息（去掉最近 K 条）末尾打断点 → 跨轮次命中
- 工具定义列表末尾打断点 → 同会话内命中

---

## 3. 现状分析：各层级缓存破坏点

### 3.1 层级 1：`tiangong-anthropic` 类型层——无法表达缓存意图

**文件**：`crates/tiangong-anthropic/src/types.rs`

```rust
// 当前：system 是单一字符串，无法分块标注
pub struct MessagesCreateRequest {
    pub system: Option<String>,        // ← 无法分块
    pub messages: Vec<Message>,
    // ...
}

// 当前：ContentBlockParam 无 cache_control 字段
pub enum ContentBlockParam {
    Text { text: String },             // ← 无 cache_control
    ToolUse { ... },                   // ← 无 cache_control
    ToolResult { ... },                // ← 无 cache_control
}

// 当前：Tool 无 cache_control 字段
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,           // ← 无 cache_control
}

// 当前：Usage 不记录缓存统计
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    // ← 缺少 cache_creation_input_tokens
    // ← 缺少 cache_read_input_tokens
}
```

**影响**：即使 Anthropic 支持显式 Prompt Caching，当前类型定义根本无法传递缓存标注，只能依赖隐式前缀缓存。

---

### 3.2 层级 2：`tiangong-llm` mapping 层——结构化信息被压缩为字符串

**文件**：`crates/tiangong-llm/src/providers/anthropic/mapping.rs`

```rust
pub(super) fn to_anthropic_request(request: &ProviderRequest) -> Result<MessagesCreateRequest, LlmError> {
    // ...
    Ok(MessagesCreateRequest {
        system: request.system.clone().filter(|v| !v.trim().is_empty()),
        // ↑ system 已是合并好的单一字符串，缓存断点信息已丢失
        // ...
    })
}
```

`ProviderRequest.system` 是 `Option<String>`，到达 mapping 层时已经是压缩后的字符串，无法再区分哪些内容是稳定的、哪些是动态的。即使将来 `tiangong-anthropic` 类型层支持 `Vec<SystemBlock>`，mapping 层也无从知道在哪里打断点。

---

### 3.3 层级 3：`tiangong-core` prompt 层——多个高波动注入点

#### 3.3.1 `memory_context` 追加到 system_prompt 尾部

**文件**：`crates/tiangong-core/src/core/mod.rs:793-800`

```rust
let mut system_prompt = assembled.final_system_prompt();
if let Some(ctx) = memory_context.as_deref() {
    system_prompt.push_str(
        "\n\n---\n## 历史上下文（回忆系统注入）\n...",
    );
    system_prompt.push_str(ctx);  // ← recall 结果每轮不同
}
```

每次 `recall_memory` 工具调用后，`memory_context` 被追加到 system prompt 末尾。由于 recall 结果内容不同，system 字符串每轮都不同，导致隐式前缀缓存完全失效。

**这是缓存命中率低的最主要原因之一。**

#### 3.3.2 MCP 工具摘要混入 system texts

**文件**：`crates/tiangong-core/src/model.rs:926-955`（`build_provider_messages`）

```rust
// 当 assembled_system_prompt 存在时：
let system_texts = if let Some(ref assembled) = req.assembled_system_prompt {
    let mut texts = vec![assembled.clone()];
    for msg in &req.context {
        if msg.role == MessageRole::System && !msg.content.trim().is_empty() {
            texts.push(msg.content.clone());  // ← System role 消息折叠进 system
        }
    }
    // ...
};
```

`PromptAssembler` 生成的 `attachment_messages` 中包含 MCP 工具摘要（`System` role），此处被提取并追加到 `system_texts`，再通过 `join("\n")` 合并成最终 system 字符串。

`build_attachments()` 中，MCP 工具摘要通过 `crate::mcp::build_mcp_tools_system_prompt(24)` 生成：

```rust
// crates/tiangong-core/src/prompt/assembler.rs
attachments.push(Message {
    id: scru128::new().to_string(),   // ← 每次新 id（不影响内容，但说明意图是每次重建）
    created_at: crate::session::now_text(),  // ← 元数据，不进 content
    content: format!("<mcp-tools>\n{mcp_text}\n</mcp-tools>"),
    // ↑ 内容本身取决于 MCP 缓存，但每次走 build_provider_messages 时
    //   都会被拼入 system 字符串
    ...
});
```

MCP 工具摘要的内容本身相对稳定（取决于 MCP 能力缓存），但因为它和其他 system 内容被合并成单一字符串，一旦其他任何部分（如 memory_context）变化，整个 system 都失效。

#### 3.3.3 System Context 折叠路径不可控

**文件**：`crates/tiangong-core/src/prompt/assembler.rs`

```
system_prompt（静态 + skills + 多媒体）
    + system_context（工作目录、允许路径）
    + memory_context（动态 recall 结果，仅在 core/mod.rs 追加）
```

最终通过 `final_system_prompt()` + `push_str(memory_context)` 合并为一个字符串。`AssembledPrompt` 虽然保留了 `system_context: Vec<String>` 和 `system_prompt: String` 的分离，但到达 `core/mod.rs` 时又被压平。稳定性信息在这一步完全丢失。

#### 3.3.4 `build_provider_messages` 降级路径存在重复 MCP 注入

**文件**：`crates/tiangong-core/src/model.rs:945`

```rust
// 当 assembled_system_prompt 为 None 时的降级路径
if let Some(mcp_tools_prompt) = build_mcp_tools_system_prompt(24) {
    texts.push(mcp_tools_prompt);  // ← 再次注入 MCP 摘要
}
```

当走降级路径（`assembled_system_prompt` 为 None）时，MCP 工具摘要会被再次注入，可能导致重复。

---

## 4. 问题汇总

| 问题编号 | 层级 | 位置 | 描述 | 严重程度 |
|---------|------|------|------|---------|
| P1 | 类型层 | `tiangong-anthropic/types.rs` | `system` 是 `Option<String>`，无法分块标注 cache_control | 🔴 关键 |
| P2 | 类型层 | `tiangong-anthropic/types.rs` | `ContentBlockParam`、`Tool` 无 `cache_control` 字段 | 🔴 关键 |
| P3 | 类型层 | `tiangong-anthropic/types.rs` | `Usage` 缺少 `cache_creation_input_tokens`、`cache_read_input_tokens` | 🟡 重要 |
| P4 | 映射层 | `tiangong-llm/mapping.rs` | system 已压缩为字符串，无法在映射时打断点 | 🔴 关键 |
| P5 | Core 层 | `core/mod.rs:793-800` | `memory_context` 追加到 system 末尾，每轮 recall 不同 | 🔴 关键 |
| P6 | Core 层 | `model.rs:930-940` | System role 消息被折叠进 system，与其他内容混合 | 🟡 重要 |
| P7 | Core 层 | `model.rs:926` | `assembled_system_prompt` 存在时，system_context 已被折叠进去，无法单独打断点 | 🟡 重要 |

---

## 5. 解决方案

### 5.1 目标：分层稳定性设计

理想的 prompt 层次结构（按稳定性从高到低）：

```
┌────────────────────────────────────────────────────────────────┐
│  System Blocks（按稳定性排列）                                    │
│                                                                │
│  [cache_control] 静态块：身份描述 + 规则（跨会话稳定，最稳定）       │
│  [cache_control] 配置块：Skills 摘要 + 多媒体能力（配置变化时才变）  │
│  [cache_control] 环境块：工作目录 + 允许路径（会话级稳定）           │
│  [无 cache]      上下文块：recall 结果（每轮可能不同）               │
└────────────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────────────┐
│  Tools（工具定义，会话内稳定）                                     │
│  [cache_control] 最后一个工具上打断点                              │
└────────────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────────────┐
│  Messages（消息列表）                                             │
│                                                                │
│  历史消息 (0 .. N-K)  [cache_control] 第 N-K 条最后内容块打断点   │
│  近期消息 (N-K .. N)  [无 cache]                                 │
│  MCP 工具摘要         [无 cache]（内容相对稳定但位置动态）           │
│  user_context        [无 cache]（记忆注入，内容可能变化）           │
│  当前用户输入          [无 cache]                                  │
└────────────────────────────────────────────────────────────────┘
```

### 5.2 方案一：`tiangong-anthropic` 类型层扩展

#### 5.2.1 新增 `CacheControl` 和 `SystemBlock`

```rust
/// 显式缓存控制标注
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: CacheControlType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheControlType {
    Ephemeral,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self { kind: CacheControlType::Ephemeral }
    }
}

/// System Prompt 块（支持显式缓存断点）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: String,  // 固定为 "text"
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl SystemBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self { kind: "text".into(), text: text.into(), cache_control: None }
    }

    pub fn with_cache(mut self) -> Self {
        self.cache_control = Some(CacheControl::ephemeral());
        self
    }
}

/// System 参数（支持纯文本和块数组两种形式）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SystemParam {
    Text(String),
    Blocks(Vec<SystemBlock>),
}
```

#### 5.2.2 更新 `MessagesCreateRequest.system`

```rust
pub struct MessagesCreateRequest {
    // ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemParam>,  // String → SystemParam
    // ...
}
```

#### 5.2.3 `ContentBlockParam` 增加 `cache_control`

```rust
pub enum ContentBlockParam {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    // ToolResult 也类似扩展
    // ...
}
```

#### 5.2.4 `Tool` 增加 `cache_control`

```rust
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}
```

#### 5.2.5 `Usage` 增加缓存统计字段

```rust
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,  // 新建缓存消耗的 token
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,       // 从缓存读取的 token（命中）
}
```

#### 5.2.6 `AnthropicClient` 开启 beta header

```rust
// AnthropicClient::request_builder() 中增加
.header("anthropic-beta", "prompt-caching-2024-07-31")
```

---

### 5.3 方案二：`tiangong-llm` 层引入结构化 System

在 `tiangong-llm` 的 `ProviderRequest` 中，将 system 从 `Option<String>` 扩展为支持分块结构：

```rust
/// System Prompt 块（携带缓存意图）
#[derive(Debug, Clone)]
pub struct SystemBlock {
    pub text: String,
    pub cached: bool,  // 是否在此块末尾打 cache_control 断点
}

pub struct ProviderRequest {
    // ...
    pub system: Option<String>,            // 保留兼容，降级用
    pub system_blocks: Vec<SystemBlock>,   // 新增：结构化 system，优先使用
    // ...
}
```

mapping 层 `to_anthropic_request()` 更新为：

```rust
pub(super) fn to_anthropic_request(request: &ProviderRequest) -> Result<MessagesCreateRequest> {
    let system = if !request.system_blocks.is_empty() {
        let blocks: Vec<AnthropicSystemBlock> = request.system_blocks.iter().map(|b| {
            AnthropicSystemBlock {
                kind: "text".into(),
                text: b.text.clone(),
                cache_control: b.cached.then(CacheControl::ephemeral),
            }
        }).collect();
        Some(SystemParam::Blocks(blocks))
    } else if let Some(ref text) = request.system {
        Some(SystemParam::Text(text.clone()))
    } else {
        None
    };

    Ok(MessagesCreateRequest {
        system,
        // ...
    })
}
```

---

### 5.4 方案三：`tiangong-core` prompt 层修复

#### 5.4.1 `memory_context` 改为消息注入，不追加到 system

**当前（core/mod.rs）**：
```rust
// 问题：每轮 recall 追加到 system，破坏缓存前缀
let mut system_prompt = assembled.final_system_prompt();
if let Some(ctx) = memory_context.as_deref() {
    system_prompt.push_str("...");
    system_prompt.push_str(ctx);
}
```

**修改后**：
```rust
// memory_context 改为注入 loop_context 消息，不修改 system
let mut loop_context_with_memory = loop_context.to_vec();
if let Some(ctx) = memory_context.as_deref() {
    // 将 recall 结果作为 System role 消息（或 User role system-reminder）注入
    loop_context_with_memory.insert(0, Message {
        role: MessageRole::System,
        content: format!(
            "<memory-recall>\n{ctx}\n</memory-recall>\n\
             注意：以上来自 recall_memory 检索结果，仅供当前回复参考，\
             请勿重复回忆，除非用户有新的回忆需求。"
        ),
        // ...
    });
}

let assembled = assembler.assemble(
    session,
    "",
    request_tools.clone(),
    engine.models_config(),
    engine.agent_config(),
    &loop_context_with_memory,  // 传入包含 memory_context 的 loop_context
);

let system_prompt = assembled.final_system_prompt();  // 不再追加 memory_context
```

**效果**：system prompt 保持稳定（跨 recall 不变），记忆上下文进入消息列表，不影响 system 缓存。

#### 5.4.2 `AssembledPrompt` 携带结构化 system blocks

修改 `AssembledPrompt` 以保留分块稳定性信息：

```rust
pub struct AssembledPrompt {
    /// System blocks（携带各块的缓存意图）
    pub system_blocks: Vec<PromptSection>,  // 已有，复用
    // 去掉 system_prompt: String（改为从 system_blocks 生成）
    pub system_context: Vec<String>,
    // ...
}

impl AssembledPrompt {
    /// 构建结构化 system blocks（携带 cached 标注）
    pub fn build_system_blocks(&self) -> Vec<SystemBlock> {
        let mut blocks = Vec::new();

        // 静态块（身份 + 规则）→ 打缓存断点
        for text in &self.static_system {
            blocks.push(SystemBlock { text: text.clone(), cached: false });
        }
        if let Some(last) = blocks.last_mut() {
            last.cached = true;  // 静态块最后一块打断点
        }

        // 动态块（Skills、多媒体）→ 打缓存断点
        for text in &self.dynamic_system {
            blocks.push(SystemBlock { text: text.clone(), cached: false });
        }
        if let Some(last) = blocks.last_mut() {
            last.cached = true;
        }

        // System context（工作目录等）→ 打缓存断点
        for text in &self.system_context {
            blocks.push(SystemBlock { text: text.clone(), cached: false });
        }
        if let Some(last) = blocks.last_mut() {
            last.cached = true;  // 最多 4 个断点，此处为第 3 个
        }

        blocks
    }
}
```

#### 5.4.3 历史消息缓存断点

在 `build_messages()` 中，对历史消息的第 (N-K) 条最后内容块打缓存断点（此功能依赖 `ContentBlockParam.cache_control` 支持后实现）：

```
历史消息数量 N，保留最近 K=6 轮（12 条消息）
在第 N-K 条消息的最后一个 content block 上打 cache_control 断点
```

#### 5.4.4 工具定义缓存断点

在 mapping 层，对工具列表的最后一个 Tool 打 `cache_control` 断点：

```rust
let tools = request.tools.iter().enumerate().map(|(i, tool)| {
    AnthropicTool {
        cache_control: (i == request.tools.len() - 1).then(CacheControl::ephemeral),
        // ...
    }
}).collect();
```

---

## 6. 消息层次结构设计（修改后完整视图）

```
Request {
  system: Blocks([
    { text: "你是天工...\n规则：...",      cached: true  },  // 静态块断点
    { text: "多媒体能力：...\nSkills：...", cached: true  },  // 动态配置块断点
    { text: "工作目录：...",               cached: true  },  // 环境块断点（第3个断点）
    // memory_context 不再在 system 中出现
  ]),

  tools: [
    { name: "read_file",  ... },
    { name: "run_command", ..., cache_control: ephemeral },  // 工具断点（第4个断点）
  ],

  messages: [
    // --- 稳定历史区（从缓存命中）---
    { role: user,      content: [{ text: "第一轮问题", cache_control: ephemeral }] },
    { role: assistant, content: [{ text: "第一轮回答" }] },
    // ...（更早的历史，最后一条打断点）

    // --- 近期活跃区（不打断点）---
    { role: user,      content: [{ text: "最近 K 轮消息..." }] },
    { role: assistant, content: [...] },

    // --- 当前轮动态区（不打断点）---
    { role: system,    content: "<memory-recall>...</memory-recall>" },  // recall 结果（若有）
    { role: system,    content: "<mcp-tools>...</mcp-tools>" },          // MCP 摘要（若有）
    { role: user,      content: "<system-reminder>...</system-reminder>" },  // user_context
    { role: user,      content: "当前用户输入" },
  ]
}
```

**注**：由于消息列表中同一 role 的相邻消息会被 Anthropic 合并，实际请求中 system role 消息需要转换为适当形式（如追加到前一条 user 消息中，或合并到 system blocks 的最后一个不缓存块）。这部分细节在实施阶段处理。

---

## 7. 实施计划

### Phase 1：类型层和映射层（前置条件）

- [ ] `tiangong-anthropic/types.rs`：新增 `CacheControl`、`SystemBlock`、`SystemParam`
- [ ] `tiangong-anthropic/types.rs`：`MessagesCreateRequest.system` 改为 `Option<SystemParam>`
- [ ] `tiangong-anthropic/types.rs`：`ContentBlockParam` 各变体增加 `cache_control: Option<CacheControl>`
- [ ] `tiangong-anthropic/types.rs`：`Tool` 增加 `cache_control: Option<CacheControl>`
- [ ] `tiangong-anthropic/types.rs`：`Usage` 增加 `cache_creation_input_tokens`、`cache_read_input_tokens`
- [ ] `tiangong-anthropic/client.rs`：`request_builder()` 添加 `anthropic-beta: prompt-caching-2024-07-31` header
- [ ] `tiangong-llm/request.rs`：`ProviderRequest` 增加 `system_blocks: Vec<SystemBlock>`
- [ ] `tiangong-llm/mapping.rs`：`to_anthropic_request()` 支持结构化 system 和 cache_control

### Phase 2：Core 层修复（核心缓存破坏点）

- [ ] `tiangong-core/src/core/mod.rs`：`memory_context` 不再追加到 system_prompt，改为注入 loop_context
- [ ] `tiangong-core/src/prompt/assembler.rs`：`PromptAssembler::assemble()` 传出结构化 system blocks
- [ ] `tiangong-core/src/prompt/types.rs`：`AssembledPrompt` 增加 `build_system_blocks()` 方法
- [ ] `tiangong-core/src/model.rs`：`ModelRequest` 支持 `system_blocks`，`build_provider_messages()` 使用结构化路径

### Phase 3：缓存断点精细化（锦上添花）

- [ ] 历史消息第 (N-K) 条打缓存断点
- [ ] 工具列表最后一个工具打缓存断点
- [ ] `TokenUsage` 携带 `cache_creation_tokens` 和 `cache_read_tokens` 并上报给前端

---

## 8. 预期效果

| 场景 | 修复前 | 修复后 |
|------|--------|--------|
| 第 2 轮问答（无 recall） | 0% 缓存 | 静态块 + 配置块 + 环境块 + 工具全部命中（约 60-80% token 缓存） |
| 第 2 轮问答（有 recall） | 0% 缓存（system 变化） | recall 进消息层，system 不变，断点前历史全部命中 |
| 第 N 轮问答（长对话） | 接近 0% | 历史消息区（N-K 条）+ system + 工具全部命中（约 80-90%） |
| 跨会话相同 session | 依赖 5min TTL | 静态块跨会话命中（TTL 内） |

---

## 9. 风险与注意事项

1. **最多 4 个缓存断点**：Anthropic 限制每请求最多 4 个 `cache_control: ephemeral` 标注，设计时已控制断点数量（静态块 1 个 + 配置块 1 个 + 环境块 1 个 + 工具/消息历史 1 个 = 4 个）。

2. **消息角色合并规则**：Anthropic 要求 user/assistant 消息交替出现。`memory_context`（system role）和 `user_context`（user role）注入时需确保不产生连续同角色消息。实施阶段需要在 `sanitize_provider_messages()` 中处理。

3. **隐式缓存回退**：`anthropic-beta: prompt-caching-2024-07-31` 仅对支持的模型有效（如 claude-3.5-sonnet、claude-3.7-sonnet 等）。对其他模型仍退化为隐式前缀缓存，此时结构化 system blocks 的 `text` 字段照常工作，只是 `cache_control` 字段会被忽略。

4. **TTL 5 分钟**：缓存 TTL 较短，长时间不活跃的会话需要重新建立缓存。此为 Anthropic 平台限制，非系统问题。

5. **cache_creation 收费**：缓存写入（cache_creation_input_tokens）按 1.25× 价格收费，缓存命中（cache_read_input_tokens）按 0.1× 价格收费。长对话和重复前缀场景下整体成本会降低，但短对话或内容差异大的场景缓存写入成本可能略有上升。

---

## 参考

- [Anthropic Prompt Caching 文档](https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching)
- `crates/tiangong-anthropic/src/types.rs`
- `crates/tiangong-llm/src/providers/anthropic/mapping.rs`
- `crates/tiangong-core/src/prompt/assembler.rs`
- `crates/tiangong-core/src/core/mod.rs`
