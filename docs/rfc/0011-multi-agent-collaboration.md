# RFC 0011: 多智能体协作系统

- **状态**: Draft
- **日期**: 2026-05-12
- **作者**: Hubert Shelley

## 1. 概述

### 1.1 背景

当前天工是单 Agent 架构——一个 ReactEngine 循环处理所有任务。面对复杂项目（如同时需要规划、开发、测试），单 Agent 存在明显瓶颈：上下文窗口有限、无法并行执行、缺少角色专业化。

### 1.2 目标

设计一套多智能体协作系统，使主 Agent 能在对话中动态组建团队，Sub Agent 之间通过工具调用互相通讯，共享工作区但通过文件编辑锁防止冲突。用户可直接与任意存活 Agent 交互。

### 1.3 核心能力

| 能力 | 描述 |
|------|------|
| 团队创建 | 主 Agent 在对话中动态创建 Sub Agent，指定角色和职责 |
| Agent 间通讯 | 通过 `send_message` / `broadcast_message` 工具互相发送消息 |
| 文件编辑锁 | 多 Agent 编辑同一文件时自动排队，防止冲突 |
| 用户交互 | 用户可通过 @提及向指定 Agent 或全体发送指令 |
| 直接推送 | Sub Agent 需要告知用户的消息直接推送到前端 |
| 视角切换 | 用户可切换查看不同存活 Agent 的执行细节 |

### 1.4 非目标

- 不做跨会话 Agent 持久化（Agent 生命周期绑定会话）
- 不做分布式多 Agent（所有 Agent 在同一进程内运行）
- 不做 Agent 市场或插件系统

---

## 2. Agent 生命周期

### 2.1 生命周期类型

```
┌─────────────────────────────────────────────┐
│                  Session                     │
│                                              │
│  Main Agent (always alive)                   │
│  ├── @pm    (persistent) ────────────────┐   │
│  ├── @dev   (persistent) ──────────────┐ │   │
│  ├── @test  (persistent) ────────────┐ │ │   │
│  └── @research (temporary) ────────┐ │ │ │   │
│         完成后自动销毁 ↑            │ │ │ │   │
│                                    │ │ │ │   │
│  用户 @提及: @pm @dev @test @all   │ │ │ │   │
│  临时 Agent: @research:xxx         │ │ │ │   │
└────────────────────────────────────┴─┴─┴─┴───┘
```

- **持久 Agent**：跨任务保持上下文，随会话存在。适合 PM、Developer、Tester 等长期角色
- **临时 Agent**：单次任务完成后自动销毁。适合 Researcher、Reviewer 等短期角色
- **主 Agent**：始终存活，负责团队管理和消息路由

### 2.2 创建流程

主 Agent 通过工具调用创建 Sub Agent：

```
Agent Call: create_agent(
  role: "developer",
  label: "Developer",
  system_prompt: "你是一个资深 Rust 开发者...",
  lifecycle: "persistent",
  tools: ["read_file", "write_file", "replace_in_file", "run_command", "search_code"]
)
```

### 2.3 @提及规则

| 语法 | 含义 |
|------|------|
| `@dev` | 向 Developer Agent 发送消息 |
| `@dev @test` | 同时向 Developer 和 Tester 发送 |
| `@all` | 向所有存活 Agent 广播 |
| `@research:xxx` | 创建临时 Agent 执行 xxx 任务 |

### 2.4 销毁

- **临时 Agent**：任务完成（ReactEngine 循环结束且无待处理消息）后自动销毁
- **持久 Agent**：主 Agent 调用 `dismiss_agent(role)` 销毁，或会话结束时统一销毁
- **强制销毁**：用户通过前端面板手动关闭

---

## 3. Agent 注册与身份

### 3.1 数据结构

```rust
/// Agent 描述符
struct AgentDescriptor {
    /// 唯一标识（会话内唯一）
    agent_id: String,
    /// 角色标识（用于 @提及）
    role: String,
    /// 显示名称
    label: String,
    /// Agent 专属系统 prompt
    system_prompt: String,
    /// 生命周期类型
    lifecycle: AgentLifecycle,
    /// 可用工具列表（从主 Agent 工具集中选取）
    tools: Vec<String>,
    /// 当前状态
    status: AgentStatus,
}

enum AgentLifecycle {
    /// 持久存在，随会话生命周期
    Persistent,
    /// 单次任务完成后自动销毁
    Temporary,
}

enum AgentStatus {
    /// 空闲，等待消息
    Idle,
    /// 正在执行任务
    Running,
    /// 等待用户输入
    WaitingForUser,
    /// 等待文件锁
    WaitingForLock,
    /// 已销毁
    Terminated,
}
```

### 3.2 Agent 注册表

```rust
/// 会话级 Agent 注册表
struct AgentRegistry {
    agents: HashMap<String, AgentDescriptor>,
    /// agent_id → ReactEngine 实例
    engines: HashMap<String, ReactEngine>,
    /// agent_id → 独立 Session
    sessions: HashMap<String, Session>,
}
```

每个 Sub Agent 拥有独立的 Session（继承父 Session 的 cwd），独立的 ReactEngine 实例（共享 RuntimeEngine）。

---

## 4. 通讯机制

### 4.1 工具定义

#### `send_message` — 定向发送

```json
{
  "name": "send_message",
  "description": "向指定 Agent 发送消息。支持 @role 格式指定目标。",
  "input_schema": {
    "type": "object",
    "properties": {
      "to": {
        "type": "string",
        "description": "目标 Agent 的 role，如 'pm'、'dev'、'test'"
      },
      "content": {
        "type": "string",
        "description": "消息内容"
      },
      "priority": {
        "type": "string",
        "enum": ["normal", "urgent"],
        "description": "消息优先级，默认 normal"
      }
    },
    "required": ["to", "content"]
  }
}
```

#### `broadcast_message` — 广播

```json
{
  "name": "broadcast_message",
  "description": "向所有存活 Agent 广播消息。",
  "input_schema": {
    "type": "object",
    "properties": {
      "content": {
        "type": "string",
        "description": "广播内容"
      },
      "exclude": {
        "type": "array",
        "items": { "type": "string" },
        "description": "排除的 Agent role 列表（通常排除自己）"
      }
    },
    "required": ["content"]
  }
}
```

#### `notify_user` — 直接推送到前端

```json
{
  "name": "notify_user",
  "description": "直接向用户推送消息，无需经主 Agent 转发。用于进度汇报、阻塞通知、提问等场景。",
  "input_schema": {
    "type": "object",
    "properties": {
      "content": {
        "type": "string",
        "description": "推送给用户的内容"
      },
      "level": {
        "type": "string",
        "enum": ["info", "warning", "error", "question"],
        "description": "消息级别，默认 info"
      }
    },
    "required": ["content"]
  }
}
```

### 4.2 消息流转

```
用户 ──@dev──→ 主 Agent ──send_message──→ Developer Agent
                                              │
                                              ├── 执行任务
                                              ├── notify_user ──→ 前端（带 Agent 标识）
                                              │
                                              └── send_message(to="test") ──→ Tester Agent
                                                                            │
                                                                            ├── 执行测试
                                                                            └── send_message(to="pm") ──→ PM Agent
```

### 4.3 消息路由

主 Agent 充当消息路由枢纽：

1. Agent 调用 `send_message(to, content)` 时，工具执行结果写入目标 Agent 的收件箱
2. 目标 Agent 在 ReactEngine 循环中检查收件箱，收到的消息作为用户消息注入上下文
3. 主 Agent 调用 `send_message` 时直接路由；Sub Agent 调用时经主 Agent 转发确认

### 4.4 消息格式

```rust
struct AgentMessage {
    /// 消息 ID
    id: String,
    /// 发送方 agent_id
    from: String,
    /// 接收方 agent_id（广播时为 "all"）
    to: String,
    /// 消息内容
    content: String,
    /// 优先级
    priority: MessagePriority,
    /// 时间戳
    created_at: String,
}
```

---

## 5. 文件编辑锁

### 5.1 设计原则

- 文件级粒度锁，Agent 编辑文件前必须获取锁
- 非阻塞式：获取失败时立即返回错误，Agent 自行决定等待或换策略
- 自动释放：Agent 任务完成或销毁时释放所有锁

### 5.2 工具定义

#### `lock_file` — 获取文件锁

```json
{
  "name": "lock_file",
  "description": "获取文件编辑锁。编辑文件前必须先获取锁，防止多 Agent 冲突。",
  "input_schema": {
    "type": "object",
    "properties": {
      "path": {
        "type": "string",
        "description": "要锁定的文件路径"
      },
      "mode": {
        "type": "string",
        "enum": ["exclusive"],
        "description": "锁模式，目前仅支持 exclusive"
      }
    },
    "required": ["path"]
  }
}
```

#### `unlock_file` — 释放文件锁

```json
{
  "name": "unlock_file",
  "description": "释放文件编辑锁。",
  "input_schema": {
    "type": "object",
    "properties": {
      "path": {
        "type": "string",
        "description": "要释放的文件路径"
      }
    },
    "required": ["path"]
  }
}
```

### 5.3 锁状态管理

```rust
struct FileLockManager {
    /// path → (agent_id, locked_at)
    locks: HashMap<String, FileLock>,
}

struct FileLock {
    /// 持有锁的 Agent
    holder: String,
    /// 锁获取时间
    locked_at: String,
}
```

### 5.4 自动集成

`write_file` 和 `replace_in_file` 工具在多 Agent 模式下自动检查文件锁：

1. Agent 调用 `write_file` / `replace_in_file`
2. 工具执行前检查当前 Agent 是否持有该文件的锁
3. 已持有 → 正常执行
4. 未持有 → 返回错误提示，Agent 需先调用 `lock_file` 或等待锁释放
5. 主 Agent 不受锁限制（拥有最高权限）

### 5.5 锁超时与死锁检测

- 默认锁超时：300 秒（5 分钟）
- 死锁检测：定期扫描锁等待图，发现环时通知主 Agent 处理

---

## 6. 任务流转示例

以「开发一个新功能并测试」为例：

```
1. 用户: "帮我实现用户认证模块"

2. Main Agent:
   - 分析任务，决定组建团队
   - create_agent(role="pm", label="Project Manager", lifecycle="persistent")
   - create_agent(role="dev", label="Developer", lifecycle="persistent")
   - create_agent(role="test", label="Tester", lifecycle="persistent")
   - send_message(to="pm", "用户需要实现用户认证模块，请规划任务")

3. PM Agent:
   - 分析需求，拆分为开发任务和测试任务
   - send_message(to="dev", "请实现用户认证模块，包括：1) 登录 API 2) JWT 中间件 3) 数据库迁移")
   - notify_user("已拆分任务，Developer 开始实现认证模块")

4. Developer Agent:
   - lock_file("src/auth/middleware.rs")
   - write_file / replace_in_file 执行开发
   - unlock_file("src/auth/middleware.rs")
   - send_message(to="test", "认证模块开发完成，请编写测试用例")
   - send_message(to="pm", "认证模块开发完成，已提测")

5. Tester Agent:
   - lock_file("tests/auth_test.rs")
   - write_file 编写测试
   - run_command 执行测试
   - unlock_file("tests/auth_test.rs")
   - send_message(to="pm", "测试完成：5 个用例全部通过")

6. PM Agent:
   - 汇总结果
   - send_message(to="main", "用户认证模块已完成，测试通过")

7. Main Agent:
   - 向用户汇报最终结果
```

---

## 7. 前端交互

### 7.1 布局设计

```
┌─────────────────────────────────────────────────┐
│  [Main] [PM] [Dev] [Test]          ← Agent Tab  │
├─────────────────────────────────────────────────┤
│                                                  │
│  当前 Tab: Dev                                   │
│  ──────────────────────                          │
│  🔒 locked: src/auth/middleware.rs               │
│  📝 write_file: src/auth/middleware.rs           │
│     + JWT 中间件实现...                          │
│  ✅ write_file 完成                              │
│  📨 send_message → @test "认证模块开发完成"       │
│                                                  │
├─────────────────────────────────────────────────┤
│  用户输入: @pm 调整一下认证策略...               │
│  [发送]                                          │
└─────────────────────────────────────────────────┘
```

### 7.2 Agent Tab

- 顶部 Tab 栏显示所有存活 Agent，带状态指示器（空闲/运行中/等待）
- 点击 Tab 切换到对应 Agent 的执行视图
- Main Tab 为默认视图，显示主 Agent 的输出和所有 Agent 的汇总消息
- Agent 销毁后 Tab 变灰但仍可查看历史

### 7.3 Sub Agent 直接推送

Sub Agent 调用 `notify_user` 时，消息直接推送到前端，携带 Agent 标识：

```json
{
  "type": "agent_notification",
  "agent_id": "dev_001",
  "agent_label": "Developer",
  "content": "发现 src/auth 模块需要重构，是否继续？",
  "level": "question"
}
```

前端根据 `agent_id` 将消息展示在对应 Agent Tab 下，同时在 Main Tab 显示摘要通知。

### 7.4 用户 @提及

用户在输入框中使用 @提及语法：

- 输入 `@dev` 后弹出 Agent 选择器
- 支持多选：`@dev @test 同时检查这两个文件`
- `@all` 广播到所有存活 Agent
- 无 @前缀时发送给主 Agent

---

## 8. StreamEvent 扩展

### 8.1 复用现有事件

```rust
// 已有，直接复用
StreamEvent::WorkerStarted { worker_id, worker_label }
StreamEvent::WorkerChunk { worker_id, worker_label, content }
StreamEvent::WorkerCompleted { worker_id, worker_label, success }
```

将 `worker_id` 语义扩展为 `agent_id`，`worker_label` 扩展为 `agent_label`。

### 8.2 新增事件

```rust
/// Agent 创建
AgentCreated {
    agent_id: String,
    role: String,
    label: String,
    lifecycle: String,
}

/// Agent 状态变更
AgentStatusChanged {
    agent_id: String,
    label: String,
    status: String, // "idle" | "running" | "waiting_for_user" | "waiting_for_lock" | "terminated"
}

/// Agent 向用户直接推送的通知
AgentNotification {
    agent_id: String,
    agent_label: String,
    content: String,
    level: String, // "info" | "warning" | "error" | "question"
}

/// Agent 间消息（前端可选展示）
AgentMessage {
    from_agent_id: String,
    from_agent_label: String,
    to_agent_id: String,
    to_agent_label: String,
    content: String,
}

/// 文件锁变更
FileLockChanged {
    path: String,
    holder_agent_id: Option<String>,
    holder_agent_label: Option<String>,
    action: String, // "locked" | "unlocked" | "timeout"
}
```

### 8.3 事件流设计

所有 Agent 的事件通过同一个 `stream_tx` 发送，前端根据事件中的 `agent_id` 路由到对应 Tab。主 Agent 的事件不携带 `agent_id`（或使用固定值 `"main"`）。

---

## 9. 架构实现

### 9.1 新增模块

```
crates/tiangong-core/src/
├── agent_team/
│   ├── mod.rs              # 模块入口
│   ├── registry.rs         # Agent 注册表
│   ├── descriptor.rs       # AgentDescriptor 定义
│   ├── message_bus.rs      # 消息路由与收件箱
│   ├── file_lock.rs        # 文件编辑锁管理
│   ├── lifecycle.rs        # Agent 生命周期管理
│   └── tools.rs            # Agent 团队工具定义（create_agent, send_message 等）
```

### 9.2 ReactEngine 改造

```rust
impl ReactEngine {
    /// Sub Agent 执行入口
    pub async fn execute_agent_turn(
        &self,
        session: &mut Session,
        agent_id: &str,
        inbox: &mut Vec<AgentMessage>,
        stream_tx: &StdSender<StreamEvent>,
        file_locks: &FileLockManager,
    ) -> TokenUsage
}
```

### 9.3 执行模型

```
                    ┌──────────────┐
                    │  Main Agent  │
                    │  ReactEngine │
                    └──────┬───────┘
                           │ create_agent
                    ┌──────┴───────┐
                    │ AgentRegistry │
                    └──┬───┬───┬───┘
                       │   │   │
              ┌────────┤   │   ├────────┐
              ▼        ▼   │   ▼        ▼
         ┌────────┐ ┌─────┴──┐ ┌────────┐
         │  PM    │ │  Dev   │ │  Test  │
         │ Engine │ │ Engine │ │ Engine │
         └───┬────┘ └───┬────┘ └───┬────┘
             │          │          │
             └──────────┼──────────┘
                        │
                 ┌──────┴──────┐
                 │ MessageBus  │
                 └──────┬──────┘
                        │
                 ┌──────┴──────┐
                 │ FileLockMgr │
                 └──────┬──────┘
                        │
                 ┌──────┴──────┐
                 │  stream_tx  │
                 └─────────────┘
```

- 每个 Sub Agent 运行在独立的 `tokio::task::spawn_blocking` 中
- 共享 `MessageBus`（消息路由）、`FileLockManager`（文件锁）、`stream_tx`（事件流）
- 通过 `Arc<Mutex<>>` 保护共享状态

### 9.4 Sub Agent 的 ReactEngine 循环

Sub Agent 的 ReactEngine 与主 Agent 共享相同的循环逻辑，差异在于：

1. **工具集**：使用 `AgentDescriptor.tools` 过滤可用工具
2. **系统 Prompt**：使用 `AgentDescriptor.system_prompt`
3. **Session**：独立的子 Session，继承父 Session 的 cwd
4. **消息注入**：收件箱中的 `AgentMessage` 作为用户消息注入上下文
5. **文件锁**：`write_file` / `replace_in_file` 前自动检查锁状态
6. **Memory**：暂不传递 memory_handle（隔离记忆上下文）

---

## 10. 实现路径

### Phase 1：基础框架（1-2 周）

- AgentDescriptor / AgentRegistry 数据结构
- `create_agent` / `dismiss_agent` 工具定义和执行
- Sub Agent ReactEngine 启动与停止
- 基本的 StreamEvent（AgentCreated / AgentStatusChanged）

**验证**：主 Agent 可创建 Sub Agent，前端显示 Agent Tab，Sub Agent 可执行基本工具调用。

### Phase 2：消息通讯（1 周）

- `send_message` / `broadcast_message` 工具
- MessageBus 消息路由
- AgentMessage StreamEvent
- 收件箱消息注入到 ReactEngine 循环

**验证**：两个 Sub Agent 可互相发送消息，消息正确到达并触发对方行动。

### Phase 3：文件编辑锁（1 周）

- FileLockManager 实现
- `lock_file` / `unlock_file` 工具
- `write_file` / `replace_in_file` 锁检查集成
- FileLockChanged StreamEvent

**验证**：两个 Agent 同时编辑同一文件时，后到者被阻塞并收到锁冲突通知。

### Phase 4：用户交互与前端（1-2 周）

- `notify_user` 工具
- AgentNotification StreamEvent
- 前端 Agent Tab 切换
- 前端 @提及输入
- 前端 Agent 面板（状态显示、手动关闭）

**验证**：Sub Agent 可直接推送消息到前端，用户可切换 Tab 查看 Agent 详情，用户可 @指定 Agent 发送指令。

### Phase 5：完善与优化（1 周）

- 临时 Agent 自动销毁
- 锁超时与死锁检测
- Agent 错误恢复
- 性能优化（并发上限、token 预算分配）

---

## 11. 安全与约束

| 约束 | 值 | 说明 |
|------|-----|------|
| 最大 Agent 数量 | 8 | 含主 Agent，防止资源耗尽 |
| Sub Agent 最大轮次 | 10 | 主循环 20 轮的一半 |
| Sub Agent 超时 | 300 秒 | 单次任务最大执行时间 |
| 文件锁超时 | 300 秒 | 自动释放防止死锁 |
| Token 预算 | 主 Agent 60% / Sub Agent 共享 40% | 防止子 Agent 耗尽 token |
| 并发上限 | 同时运行 4 个 Sub Agent | 限制并发 tokio 任务数 |

### 权限继承

- Sub Agent 继承主 Agent 的 TrustMode
- Sub Agent 的工具权限受 `AgentDescriptor.tools` 限制
- 主 Agent 拥有文件锁最高权限（可强制释放任何锁）

---

## 12. 与现有架构的兼容性

### 复用

| 组件 | 复用方式 |
|------|----------|
| `ReactEngine` | Sub Agent 直接使用，仅注入不同的 session/tools/prompt |
| `RuntimeEngine` | 所有 Agent 共享（clone），复用 model client / tool executor |
| `Session` | Sub Agent 使用独立子 Session，`parent_session_id` 关联父 Session |
| `StreamEvent` | 扩展 Worker* 事件语义 + 新增 Agent* 事件 |
| `worker_id` | Message 的 `worker_id` 字段复用为 `agent_id` |
| `LocalToolExecutor` | Sub Agent 共享，增加锁检查层 |
| `PermissionGate` | Sub Agent 继承主 Agent 的权限配置 |

### 不修改

- `tiangong-memory`：Sub Agent 暂不传递 memory_handle
- `tiangong-types`：仅扩展 StreamEvent 枚举，不破坏现有事件
- `tiangong-connector` / `tiangong-server`：多 Agent 事件通过同一 stream 透传
- MCP 协议：Sub Agent 可使用 MCP 工具，走现有路由
