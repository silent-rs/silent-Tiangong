# RFC 0005：事件驱动循环运行时

> 状态：草稿
> 日期：2026-04-05
> 关联：`docs/desktop-agent-technical-architecture.md` §4 运行时核心数据流

## 1. 问题

当前运行时采用 **Turn-based** 模型：每条用户消息触发一个独立的 `TurnRunner`，执行完毕后退出，等待下一条消息创建新 Turn。

```
用户消息 → [TurnRunner 生命周期开始]
  Init → ContextAssembly → LlmCalling → [工具循环] → Responding
[TurnRunner 生命周期结束] → 等待下一条用户消息
```

这带来以下限制：

1. **用户消息不是事件**：用户追加消息只能通过 `ControlSignal::UserMessage` 塞入 `pending_user_messages`，但当前代码没有消费它（只在 WaitingApproval 中处理了 PermissionResponse）
2. **后台任务无法回流**：后台任务完成后无法触发新的 LLM 调用，因为没有运行中的 TurnRunner
3. **意图分类多余**：需要额外的 LLM 调用判断"聊天还是工具"，而执行层本身就能判断
4. **Turn 之间断裂**：每个 Turn 独立构建上下文，无法感知"上一个 Turn 还在后台运行"

## 2. 目标

将运行时改为 **事件驱动循环**（Event Loop）模型：

- 用户消息只是事件的一种，不再是执行的起点
- 所有事件统一进入循环：用户消息、工具结果、后台任务完成、权限响应、系统信号
- 循环持续运行，直到 LLM 判断"满足条件"
- 用户可以在循环运行期间随时追加消息，消息作为新上下文注入下一轮

## 3. 目标模型

```
         ┌─────────────────────────────────────────┐
         │           Event Loop（会话级）            │
         │                                         │
  事件 ──→  收集事件 → 组织上下文 → LLM 调用 ──→ 满足? ──→ 输出回复
  来源：    │              ↑                    │ 不满足
  · 用户消息 │              │                    ↓
  · 工具结果 │              └──── 执行工具 ←── 工具调用
  · 后台完成 │              ↑
  · 权限响应 │     用户追加消息（注入上下文）
  · 系统信号 │
         └─────────────────────────────────────────┘
```

### 3.1 核心循环伪代码

```rust
/// EventLoopRunner 不是常驻线程。
/// 由事件驱动：有事件时被唤起执行，处理完毕后挂起，资源归还。
fn run(mut self) {
    loop {
        // 1. 收集待处理事件（非阻塞，可能有多个）
        let events = collect_pending_events(&self.event_rx);

        // 2. 无事件且无待处理工作 → 挂起（不是阻塞等待，是真正释放线程）
        if events.is_empty() && !self.has_pending_work() {
            self.suspend();  // 保存状态，释放线程
            return;          // 线程退出，下次有事件时由外部重新唤起
        }

        // 3. 将事件注入上下文
        for event in &events {
            match event {
                Event::UserMessage(msg) => self.context.append_user(msg),
                Event::ToolResult(result) => self.context.append_tool_result(result),
                Event::BackgroundTaskDone(task) => self.context.append_task_result(task),
                Event::PermissionResponse(resp) => self.context.resolve_approval(resp),
                Event::Cancel => { self.cancel(); return; },
            }
        }

        // 4. 组织上下文（历史裁剪、压缩、记忆注入）
        let assembled = self.context.assemble();

        // 5. LLM 调用
        let response = llm.call(assembled)?;

        // 6. 判断结果
        if response.has_tool_calls() {
            // 不满足 → 执行工具，结果作为新事件回注到自身队列
            for call in response.tool_calls {
                let result = execute_tool(call);
                self.event_tx.send(Event::ToolResult(result));
            }
            // 继续循环处理工具结果
        } else {
            // 满足 → 输出回复
            output.send(response.text);
            // 不退出，回到步骤 1 检查是否还有事件
            // 如果没有 → 步骤 2 挂起
        }
    }
}
```

### 3.2 会话活跃状态管理

EventLoopRunner **不是常驻线程**，它有三种状态：

```
                 事件到达
  Suspended ──────────────→ Running
  (无线程,                    (持有线程,
   状态持久化)                  处理事件)
      ↑                         │
      │    无事件且无待处理工作    │
      └──────────────────────────┘

                 超时/手动
  Running ───────────────→ Stopped
                           (资源释放,
                            状态持久化到磁盘)
```

**Suspended（挂起）**：
- 内存中保留会话上下文快照（`LoopState`）
- 不持有线程、不持有 channel
- 新事件到达时，从快照恢复并启动线程

**Running（运行）**：
- 持有工作线程、event channel
- 处理事件循环
- 无事件且无待处理工作时 → 自动转为 Suspended

**Stopped（停止）**：
- 长时间不活跃（如 30 分钟无事件）→ 从内存移除
- 状态持久化到磁盘（`~/.tiangong/tasks/`）
- 下次打开会话时从磁盘恢复

```rust
/// 会话级循环状态（可挂起/恢复）
struct LoopState {
    session_id: String,
    /// 累积的循环上下文（工具结果、中间消息等）
    loop_context: Vec<Message>,
    /// 已装配的工具定义（避免重复初始化）
    tools_snapshot: Option<ToolsSnapshot>,
    /// 累积的 token 使用量
    accumulated_usage: TokenUsage,
    /// 当前轮次
    round: usize,
    /// 挂起时间
    suspended_at: Option<String>,
}

/// 活跃会话管理器
struct ActiveLoops {
    /// 运行中的 loop（持有线程）
    running: HashMap<String, RunningLoop>,
    /// 挂起的 loop（仅状态，无线程）
    suspended: HashMap<String, LoopState>,
}

impl ActiveLoops {
    /// 发送事件到会话，自动唤起挂起的 loop
    fn send_event(&mut self, session_id: &str, event: LoopEvent) {
        if let Some(running) = self.running.get(session_id) {
            // 运行中：直接注入事件
            running.event_tx.send(event);
        } else if let Some(state) = self.suspended.remove(session_id) {
            // 挂起：恢复并启动
            let loop_runner = EventLoopRunner::resume(state);
            loop_runner.event_tx.send(event);
            self.running.insert(session_id.to_string(), loop_runner.start());
        } else {
            // 不存在：创建新 loop
            let loop_runner = EventLoopRunner::new(session_id);
            loop_runner.event_tx.send(event);
            self.running.insert(session_id.to_string(), loop_runner.start());
        }
    }

    /// loop 自行挂起时的回调
    fn on_suspended(&mut self, session_id: &str, state: LoopState) {
        self.running.remove(session_id);
        self.suspended.insert(session_id.to_string(), state);
    }

    /// 定期清理长时间挂起的 loop（释放内存）
    fn cleanup_inactive(&mut self, max_idle_secs: u64) {
        self.suspended.retain(|_, state| {
            // 超时的持久化到磁盘并移除
            !is_expired(state, max_idle_secs)
        });
    }
}
```

### 3.2 与当前 TurnRunner 的关键差异

| 维度 | 当前 TurnRunner | 目标 EventLoop |
|------|---------------|---------------|
| 生命周期 | 一条用户消息 → 一个 Turn | 会话级，持续运行 |
| 用户消息 | 触发 Turn 的起点 | 事件之一，注入上下文 |
| 追加消息 | `pending_user_messages`（未消费） | 直接注入下一轮上下文 |
| 工具结果 | Turn 内部 `loop_messages` | 统一事件，与用户消息同等对待 |
| 后台任务 | Turn 外部，无法回流 | 完成事件进入循环 |
| 空闲态 | 不存在（Turn 结束即销毁） | 挂起（释放线程，保留状态） |
| 意图分类 | 需要前置 LLM 分类 | 不需要，LLM 自行判断 |
| 多轮连续 | 用户每条消息创建新 Turn | 同一循环内连续处理 |

### 3.3 事件类型

统一使用 `RuntimeEvent`（已在 `event.rs` 中定义），扩展如下：

```rust
enum LoopEvent {
    /// 用户消息
    UserMessage { content: String, attachments: Vec<Attachment> },
    /// 工具执行结果
    ToolResult { call_id: String, result: ToolResult },
    /// 后台任务完成
    BackgroundTaskDone { task_id: String, result: TaskResult },
    /// 权限审批响应
    PermissionResponse { request_id: String, approved: bool },
    /// 用户取消
    Cancel,
    /// 系统信号（配置变更、关闭等）
    SystemSignal(SystemSignalKind),
}
```

### 3.4 完成条件与挂起

LLM 返回无工具调用的文本回复时，视为当前轮"满足"：

1. 输出回复给用户
2. 回到步骤 1 检查是否还有待处理事件
3. **如果事件队列为空且无待处理工作 → 挂起**（释放线程，保留状态快照）
4. 下次有事件到达时自动恢复

特殊情况：
- LLM 返回空文本且无工具调用 → 视为满足（空回复），检查后续事件
- 连续 N 轮工具调用（MAX_ROUNDS）→ 强制进入"总结"模式
- 用户发送 Cancel → 取消当前处理，清空待处理工具调用，但**不销毁 loop 状态**
- 长时间无事件（如 30 分钟）→ 从内存移除，状态持久化到磁盘
- 应用退出 → 触发优雅关闭（见 §3.7）

### 3.5 上下文管理

循环是会话级的，上下文**持续积累**而非每次 Turn 重建：

```
会话上下文 = 历史消息（含压缩摘要）
           + 当前活跃的工具结果
           + 待处理的后台任务通知
           + 用户偏好/记忆
           + 环境信息（工作目录、可用工具）
```

每轮 LLM 调用前，上下文装配器根据 token 预算裁剪历史。

### 3.6 运行模式

CLI 和 GUI/Server 的会话管理方式不同，EventLoop 需要适配两种模式：

#### CLI 模式：单会话

```
┌─────────────────────────────────┐
│         CLI 进程                 │
│                                 │
│  ┌───────────────────────────┐  │
│  │  EventLoopRunner（唯一）   │  │
│  │  session: 当前会话         │  │
│  │  event_rx ← REPL 输入     │  │
│  │  output_tx → 终端输出      │  │
│  └───────────────────────────┘  │
│                                 │
│  REPL 循环：                     │
│    读取输入 → send_event         │
│    poll output → 打印到终端      │
└─────────────────────────────────┘
```

- 进程内**只有一个** EventLoopRunner，绑定当前会话
- 不需要 `ActiveLoops` 管理器，直接持有 loop
- `/new` 切换新会话时：挂起当前 loop → 创建新 loop
- 进程退出 = 会话结束，挂起/持久化逻辑与 GUI 一致
- 不存在"不活跃"问题 — CLI 进程活着就是活跃的

#### GUI/Server 模式：多会话

```
┌──────────────────────────────────────────────┐
│         GUI / Server 进程                      │
│                                              │
│  ┌────────────────────────────────────────┐  │
│  │         ActiveLoops 管理器              │  │
│  │                                        │  │
│  │  running:                              │  │
│  │    session-A → EventLoopRunner (线程)   │  │
│  │                                        │  │
│  │  suspended:                            │  │
│  │    session-B → LoopState (仅内存)       │  │
│  │    session-C → LoopState (仅内存)       │  │
│  │                                        │  │
│  └────────────────────────────────────────┘  │
│                                              │
│  前端/API：                                    │
│    send_event(session_id, event)             │
│    → ActiveLoops 自动路由                     │
│                                              │
│  定时器：                                      │
│    cleanup_inactive() 清理超时会话             │
└──────────────────────────────────────────────┘
```

- 通过 `ActiveLoops` 管理所有会话的 loop（running / suspended / stopped）
- 用户切换会话时：前台会话的 loop 保持 running，其他自然挂起
- 同一时刻可能有多个 running loop（多窗口 / API 并发请求）
- 不活跃会话超时后自动 stopped，释放内存

#### 统一接口

无论 CLI 还是 GUI，外部调用方通过统一接口与 EventLoop 交互：

```rust
trait LoopHost {
    /// 向会话发送事件（自动唤起挂起的 loop）
    fn send_event(&mut self, session_id: &str, event: LoopEvent);
    /// 轮询输出事件
    fn poll_output(&mut self, session_id: &str) -> Vec<TurnEvent>;
    /// 优雅关闭所有 loop
    fn shutdown_all(&mut self);
}

/// CLI 实现：单会话，直接持有 loop
struct CliLoopHost { loop_runner: Option<EventLoopRunner> }

/// GUI/Server 实现：多会话，ActiveLoops 管理
struct MultiLoopHost { active_loops: ActiveLoops }
```

### 3.7 优雅关闭

应用退出时（用户关闭窗口、`Ctrl+C`、`server stop` 等），必须将所有活跃 loop 的状态持久化到磁盘，确保下次启动可恢复。

**关闭流程**：

```
应用收到退出信号
       │
       ▼
ActiveLoops::shutdown_all()
       │
       ├── Running loop：
       │     1. 向 event_rx 发送 LoopEvent::SystemSignal(Shutdown)
       │     2. loop 收到后停止当前 LLM 调用（如有）
       │     3. 保存 LoopState 到磁盘
       │     4. 线程退出
       │
       ├── Suspended loop：
       │     1. 直接将内存中的 LoopState 写磁盘
       │     2. 从内存移除
       │
       └── 等待所有 Running loop 退出（超时 5 秒强制终止）
              │
              ▼
         应用退出
```

**持久化内容**（`~/.tiangong/loops/{session_id}.json`）：

```rust
#[derive(Serialize, Deserialize)]
struct PersistedLoopState {
    /// 会话 ID
    session_id: String,
    /// 循环上下文（工具结果、中间消息等，不含完整历史 — 历史在 session 文件中）
    loop_context: Vec<Message>,
    /// 累积 token 使用量
    accumulated_usage: TokenUsage,
    /// 当前轮次
    round: usize,
    /// 待处理的工具调用（Running 被中断时可能有）
    pending_tool_calls: Vec<PendingToolCall>,
    /// 待处理的事件（队列中还没消费的）
    pending_events: Vec<LoopEvent>,
    /// loop 被中断时的阶段
    interrupted_phase: LoopPhase,
    /// 持久化时间
    persisted_at: String,
}
```

**启动恢复**：

```
应用启动
    │
    ▼
扫描 ~/.tiangong/loops/*.json
    │
    ├── 每个文件加载为 PersistedLoopState
    │
    ├── interrupted_phase 为 Running（被强制中断）：
    │     → 标记未完成的工具调用为失败
    │     → 将中断信息作为系统消息注入 loop_context
    │     → 放入 suspended 状态（等待用户激活）
    │
    ├── interrupted_phase 为 WaitingApproval：
    │     → 恢复审批状态（等待用户激活后继续）
    │
    └── 其他（Suspended / Idle）：
          → 直接放入 suspended 状态
```

**关键原则**：
- 退出时**不丢弃任何中间状态**，宁可多写也不丢数据
- 恢复时**不自动唤起** loop，等用户主动操作该会话时再恢复
- 持久化文件与 session 文件独立（loop 状态是运行时临时数据，session 是持久数据）

## 4. 与现有架构的关系

### 4.1 替代的组件

| 现有组件 | 目标 | 处理方式 |
|---------|------|---------|
| `TurnRunner` | `EventLoopRunner` | 替代，核心重写 |
| `TurnPhase` | `LoopPhase` | 简化（Idle / Processing / WaitingApproval） |
| `QueryClassifier` | 删除 | LLM 自行判断 |
| `ControlSignal` | `LoopEvent` | 合并到统一事件 |
| `TurnEvent` | 保留 | 仍用于内部→外部的输出事件 |

### 4.2 保留的组件

| 组件 | 原因 |
|-----|------|
| `ContextAssembler` / `ContextOrganizer` | 上下文装配逻辑不变 |
| `PermissionGate` | 权限检查逻辑不变 |
| `RuntimeEngine` | 工具执行、LLM 调用逻辑不变 |
| `TurnEvent` | 输出事件流（Chunk/LlmOutput/ToolExecution）不变 |
| `TaskCoordinator` / `Worker` | 多代理逻辑不变，Worker 结果作为事件回流 |

### 4.3 修改的组件

| 组件 | 改动 |
|-----|------|
| `app_state/services/turn/` | `start_turn` 改为 `send_event`，不再每次创建线程 |
| `app_state/facade/sessions/turn_control.rs` | `poll_pending_turn` 改为 `poll_active_loop` |
| `PendingTurn` | 改为 `ActiveLoops`，管理所有会话的 running / suspended 状态 |

## 5. 实施策略

### Phase A：EventLoopRunner 核心 + 挂起/恢复

1. 新建 `src/event_loop/mod.rs`，定义 `LoopEvent`、`LoopState`、`EventLoopRunner`
2. `EventLoopRunner` 持有 `RuntimeEngine`、`Session`、`LoopState`
3. 实现核心循环：收集事件 → 组织上下文 → LLM 调用 → 判断 → 工具执行/输出
4. 实现挂起：无事件时保存 `LoopState`，释放线程退出
5. 实现恢复：从 `LoopState` 重建 `EventLoopRunner`，注入新事件继续

### Phase B：ActiveLoops 管理器

1. 新建 `ActiveLoops`，管理所有会话的 loop 状态（running / suspended）
2. `send_event` 自动处理：运行中→注入、挂起→恢复、不存在→创建
3. `start_turn` 改为 `send_event`，不再每次创建线程
4. `poll_pending_turn` 改为 `poll_active_loop`

### Phase C：生命周期管理与持久化

1. 定期清理长时间挂起的 loop（从内存移除，状态写磁盘）
2. 实现 `ActiveLoops::shutdown_all()` 优雅关闭：Running → 中断保存、Suspended → 直接写盘
3. 应用退出时注册 shutdown hook 调用 `shutdown_all()`
4. 应用启动时扫描 `~/.tiangong/loops/` 恢复未完成的 loop 状态
5. 后台任务完成事件可唤起已挂起的 loop

### Phase D：清理旧代码

1. 迁移完成后删除 `TurnRunner`
2. 删除 `QueryClassifier`（已完成）
3. 简化 `TurnPhase` / `TurnEvent` / `ControlSignal`

## 6. 风险与约束

- **内存**：挂起的 loop 仅保留 `LoopState`（上下文快照），运行中的 loop 持有完整引擎；超时后从内存移除
- **线程**：挂起时不持有线程，只有 Running 状态的 loop 占线程；不会空转
- **兼容性**：GUI/CLI/Server 的 poll 接口需要适配，但 `TurnEvent` 输出流不变
- **并发**：同一会话同一时刻只有一个 Running loop，多会话可并行
- **恢复一致性**：挂起/恢复时工具定义可能变化（MCP 热更新），恢复时需重新装配工具
- **关闭时序**：Running loop 中 LLM 调用可能正在进行，shutdown 需等待或中断；设 5 秒超时兜底
- **持久化幂等**：同一 loop 可能被多次持久化（挂起→写盘、退出→再写盘），以最后一次为准

## 7. 不在此 RFC 范围内

- 多代理协调的事件化（Worker 仍可作为 loop 内的子任务同步执行）
- 跨会话事件路由（每个 loop 独立于一个会话）
- 远程事件接入（Connector 事件先转为 LoopEvent 再注入）
