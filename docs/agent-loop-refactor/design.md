# Agent Core 现行架构

> 状态：现行说明。
>
> 本文只描述当前 `crates/tiangong-core` 已实现的结构。已删除的常驻 Driver、
> Agent Inbox、统一常驻命令通道等迁移方案不属于当前架构，不应作为代码审查或功能设计依据。

## 1. 执行模型

一句话概括：**每个活跃 turn 对应一个 turn task；空闲用户消息创建 task，运行中输入投递到该 task 自有的命令通道。**

当前没有常驻 Agent Driver，也没有 Agent Inbox：

- `shared_runtime` 提供进程级 Tokio runtime，并按 `session_id` 登记活跃的 turn task；
- 每个 task 持有本轮 `TurnContext` 和自己的命令接收端；
- task 结束后注册表条目和命令通道随之失效；
- `is_running` 仅表示注册表中是否存在尚未结束的 task。

```text
GUI / CLI / Server / 插件
        │
        ▼
TiangongCore::deliver
        │
        ├─ 用户消息，当前空闲
        │    └─ start_user_turn
        │         ├─ 从最新 Session 构建 TurnContext
        │         ├─ 保存并确认用户消息
        │         └─ shared_runtime::spawn_turn → react::turn::run_turn
        │
        ├─ 用户消息，当前运行中
        │    └─ shared_runtime::send_command(InjectUserMessage)
        │         └─ 当前 turn 中断活动、保存消息并从新意图重新分析
        │
        └─ 其他活动期输入
             └─ 发送到当前 turn 的命令通道；无活跃 turn 时按输入类型拒绝或延迟落盘
```

## 2. 关键模块

| 模块 | 当前职责 |
| --- | --- |
| `core/mod.rs` | `TiangongCore` 对外入口；加载 Session、构建 `TurnContext`、空闲起轮、运行中投递命令、空闲期维护与关闭 |
| `shared_runtime.rs` | 共享 Tokio runtime；turn task 注册、启动、命令投递、活动查询、取消与等待 |
| `turn_context.rs` | 单个 turn 所需的 Session、模型客户端、插件、工具、配置与事件发送端 |
| `react/turn.rs` | 单轮生命周期、执行结果提交、插件通知、终态和最终持久化 |
| `react/execute.rs` | 当前 turn 内的 Agent Loop：模型请求、工具调用、阶段切换和命令仲裁 |
| `react/command.rs` | 活动 turn 的命令处理；用户引导消息通过 `save_user_message_and_restart` 保存并重启分析 |
| `react/tools.rs` | 工具批次、权限/审批、执行、结果记录和工具协议闭合 |
| `react/request.rs` / `compression.rs` | 模型请求策略、上下文压力处理和压缩 |
| `session.rs` | 会话消息、状态与持久化格式 |

## 3. Agent Loop

当前 Agent Loop 在一个 turn task 内运行：

1. 根据最新 Session 构建模型请求；
2. 调用模型；
3. 有工具调用时执行工具并写入结果，再请求模型；
4. 无工具调用时形成候选结果并进入收尾；
5. 取消、用户引导、工具注入和运行配置变化通过本 turn 的命令通道处理。

Loop 不是常驻任务。一个 turn 完成后 task 结束；下一条空闲用户消息会重新加载最新 Session 并创建新的 turn task。

## 4. 用户引导消息

用户消息使用同一个 `AgentInputKind::Message` 入口，但按当前活动状态分流：

### 当前空闲

`TiangongCore::start_user_turn`：

1. 从磁盘加载最新 Session；
2. 构建本轮上下文；
3. 保存用户消息，成功后发送确认事件；
4. 创建 turn task 并进入 `run_turn`。

### 当前运行中

消息被转换为 `Command::InjectUserMessage` 并发送到当前 turn：

1. 中断当前模型请求、工具批次或其他活动；
2. 闭合需要闭合的工具协议；
3. 保存新用户消息；
4. 清理当前新意图不应继承的工具历史；
5. 将阶段切回模型请求，从最新意图重新分析。

### 收尾窗口

如果消息已进入命令通道但本轮来不及消费，task wrapper 会排空剩余用户消息：消息不会静默丢失，最后一条可作为后续 turn 的起点，其余保存为历史。

## 5. 其他输入的当前路由

- **工具注入**：活动时投递当前 turn；空闲时写入 Session 延迟队列，由下一轮消费。
- **审批和交互响应**：当前实现仍投递当前 turn 的命令通道，并由工具流水线等待；这是现状，不代表未来插件化方案已经定型。
- **取消与关闭**：通过当前 task 的命令通道取消，并由 `shared_runtime` 等待 task 结束。
- **手动压缩**：空闲时占用本会话 task 槽；用户消息到达会取消压缩并创建新 turn。
- **插件反馈**：绑定本轮命令发送端；turn 结束、通道关闭后，迟到反馈发送失败。

## 6. 设计与审查约束

1. 不得假设存在 Agent Inbox、常驻 Driver 或跨 turn 的统一命令接收循环。
2. 讨论新功能时应明确它发生在：空闲起轮、活动 turn 命令处理、turn 收尾，还是 Session 持久化边界。
3. 后续 turn 必须在真正启动时读取最新 Session，不提前持有未来上下文快照。
4. 新的用户响应若计划复用“引导消息逻辑”，应具体指复用 `AgentInputKind::Message` 的状态分流及 `InjectUserMessage`/`start_user_turn` 路径，而不是引用不存在的 Inbox/Driver。
5. 被推翻的迁移设计只能从 Git 历史追溯，不得重新写入当前架构或完成标准。
