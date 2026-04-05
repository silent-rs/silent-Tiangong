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
loop {
    // 1. 收集事件（非阻塞，可能有多个）
    let events = collect_pending_events(&event_rx);

    // 2. 判断是否需要行动
    if events.is_empty() && !has_pending_work() {
        // 空闲态：阻塞等待下一个事件
        let event = event_rx.recv()?;
        events.push(event);
    }

    // 3. 将事件注入上下文
    for event in &events {
        match event {
            Event::UserMessage(msg) => context.append_user(msg),
            Event::ToolResult(result) => context.append_tool_result(result),
            Event::BackgroundTaskDone(task) => context.append_task_result(task),
            Event::PermissionResponse(resp) => context.resolve_approval(resp),
            Event::Cancel => return,
            // ...
        }
    }

    // 4. 组织上下文（历史裁剪、压缩、记忆注入）
    let assembled = context.assemble();

    // 5. LLM 调用
    let response = llm.call(assembled)?;

    // 6. 判断结果
    if response.has_tool_calls() {
        // 不满足 → 执行工具，结果作为事件回注
        for call in response.tool_calls {
            let result = execute_tool(call);
            event_tx.send(Event::ToolResult(result));
        }
    } else {
        // 满足 → 输出回复，回到空闲态等待下一个事件
        output.send(response.text);
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
| 空闲态 | 不存在（Turn 结束即销毁） | 阻塞等待事件 |
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

### 3.4 完成条件

LLM 返回无工具调用的文本回复时，视为"满足"。循环不退出，回到空闲态等待下一个事件。

特殊情况：
- LLM 返回空文本且无工具调用 → 视为满足（空回复）
- 连续 N 轮工具调用（MAX_ROUNDS）→ 强制进入"总结"模式
- 用户发送 Cancel → 立即终止当前处理，回到空闲态

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
| `app_state/facade/sessions/turn_control.rs` | `poll_pending_turn` 改为 `poll_event_loop` |
| `PendingTurn` | 改为 `ActiveLoop`，生命周期=会话 |

## 5. 实施策略

### Phase A：EventLoopRunner 核心

1. 新建 `src/event_loop/mod.rs`，定义 `LoopEvent` 和 `EventLoopRunner`
2. `EventLoopRunner` 持有 `RuntimeEngine`、`Session`、上下文状态
3. 实现核心循环：收集事件 → 组织上下文 → LLM 调用 → 判断 → 工具执行/输出
4. 用户消息通过 `event_tx.send(LoopEvent::UserMessage(...))` 注入

### Phase B：会话级生命周期

1. `TiangongState` 为每个活跃会话维护一个 `EventLoopRunner`
2. 用户切换会话时，旧会话的 loop 进入空闲态（不销毁）
3. `start_turn` 改为 `send_message`：如果 loop 已存在，直接注入事件；否则创建

### Phase C：后台任务回流

1. 后台任务完成时发送 `LoopEvent::BackgroundTaskDone`
2. 空闲态的 loop 收到事件后自动唤醒，组织上下文通知用户

### Phase D：清理旧代码

1. 迁移完成后删除 `TurnRunner`
2. 删除 `QueryClassifier`（已完成）
3. 简化 `TurnPhase` / `TurnEvent` / `ControlSignal`

## 6. 风险与约束

- **内存**：每个活跃会话维护一个 loop，需要控制空闲 loop 的内存占用
- **线程**：loop 在空闲时阻塞在 `event_rx.recv()`，不消耗 CPU
- **兼容性**：GUI/CLI/Server 的 poll 接口需要适配，但 `TurnEvent` 输出流不变
- **并发**：同一会话不会有并发 loop，但多会话可能并行运行

## 7. 不在此 RFC 范围内

- 多代理协调的事件化（Worker 仍可作为 loop 内的子任务同步执行）
- 跨会话事件路由（每个 loop 独立于一个会话）
- 远程事件接入（Connector 事件先转为 LoopEvent 再注入）
