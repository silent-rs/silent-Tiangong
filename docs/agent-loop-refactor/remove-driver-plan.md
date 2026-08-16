# 移除常驻 Driver 规划（回归 spawn 持有 TurnContext 模型）

日期：2026-08-16
状态：已完成（2026-08-16）。实施记录：终态发布回归 turn 内部（每轮独立 Done）；压缩期间用户消息改为取消压缩立即起轮；连发消息收尾排空后仅最后一条接续起轮、其余保存为历史；Drop 改为空操作。详见 design.md v2。

## 1. 背景与动机

当前分支（feature/agent-execution-core）将 Agent 执行收敛为"唯一通道 + 常驻 Driver"。
实际评审结论：

- 产品主场景中一个 Core 对应一个会话，界面输入天然串行，Driver 消除的竞争窗口
  （查空闲与启动之间的交错、并发投递）几乎不可触发；
- "保证单一会话写入者"并非 Driver 的功劳：主分支同样有单写者协调
  （各方法的双分支 + turn task 自有命令通道），两种模型只是协调形式不同；
- Driver 换来的主要是理论健壮性与测试便利，代价是 inbox.rs（431 行）、
  drive_agent、running 标志、终态暂存仲裁（try_recv_or_finish）这一整层结构，
  可理解性差、概念数量多。

因此决定：**移除常驻 Driver，回到"每个 turn 一个任务、任务持有 TurnContext、
通道跟随任务"的模型**，同时保留本分支中有真实价值的行为修复。

## 2. 目标模型

```
deliver(用户消息)——空闲起轮、运行中投通道:
  会话运行中 → send_command(InjectUserMessage) → turn 内引导路径
               （中断活动 → 保存消息 → 从新意图重启，已有实现，react/command.rs）
  会话空闲   → Core 直接起轮：
               build_turn_context（每轮从磁盘取最新，保留 ALR-201/204）
               → 保存消息 → 发确认事件 → spawn turn task（持有 ctx + 命令通道）

空闲期操作（压缩/清空/改标题/改配置）: Core 直接执行；运行中则投通道由 turn 处理
running 查询: 注册表中该 session 的任务句柄是否存活（无独立标志位）
关闭: 发 Cancel → join 任务 → 通道内未处理用户消息落盘（不丢）
```

关键取舍：

- turn 进行中的新消息走**引导**（不打断丢弃），优于主分支的"取消 + 忙等 5 秒"；
  引导逻辑在 turn 内部（react/command.rs），与 Driver 无关，整体保留。
- 空闲直启的启动窗口由 spawn_turn 内部双检兜底（查表 + 已存在则 abort 新任务 +
  generation 防误删），即主分支已验证的模式。
- 终态发布回到 turn 收尾内部完成：没有排队概念，"连续任务间中间终态"问题
  （8389be69 修复的对象）在新模型下自然消失。

## 3. 保留清单（本分支有价值的行为，不随 Driver 删除）

| 项 | 位置 | 说明 |
| --- | --- | --- |
| turn 内执行循环与命令仲裁 | react/execute.rs、command.rs、interrupt.rs、tools.rs、phase.rs、request.rs | 引导/审批/取消/工具注入的整套协议，与 Driver 无关 |
| 消息保存确认语义 | run_turn 前置（原 ALR-202） | 保存成功才向界面确认 |
| 终态锚点写入最新用户消息 | turn.rs（原 ALR-107） | 引导后状态归位 |
| 每轮从磁盘构建上下文 | TiangongCore::build_turn_context（已挂回 Core） | 不提前构造快照 |
| 插件反馈封口 | core/plugin/feedback.rs | 活动结束后拒绝过期插件反馈 |
| 关闭时未处理消息落盘 | persist_pending_on_shutdown 逻辑 | 搬到关闭路径，不丢消息 |
| turn 收尾测试冻结点 | core/test_support barrier | 测试同步能力 |

## 4. 删除清单

| 项 | 位置 |
| --- | --- |
| AgentScheduling / SchedulingState / running 标志 | react/inbox.rs |
| 全局 AGENTS 注册表与 ensure_agent_session / remove_agent | react/inbox.rs |
| try_recv_or_finish（终态暂存仲裁） | react/inbox.rs |
| deliver_to_active（活动期输入守门） | react/inbox.rs |
| 常驻 drive_agent 循环与 terminal_event 暂存 | core/mod.rs |
| TurnSpawner（已删除，本次对话完成） | core/mod.rs |

## 5. 重建清单（参考主分支 shared_runtime.rs 的成熟模式）

| 项 | 说明 |
| --- | --- |
| TurnTask 注册表 | session_id → { generation, cmd_tx, JoinHandle }；HashMap + Mutex |
| spawn_turn | 命令通道创建、插件 feedback 注入、双检 + abort、oneshot 启动门、generation 防误删 |
| is_running / is_alive | 注册表条目 + !handle.is_finished() |
| send_command | 向活跃任务投递命令（turn.rs:220、lite 路径依赖） |
| cancel_and_join | 关闭：Cancel + 阻塞 join + 通道排空落盘 |
| CommandIngress 迁居 | 插件反馈封口所需的发送端包装搬到 plugin/feedback 或 shared_runtime |

## 6. 迁移步骤（每步独立可编译、测试可跑）

1. **重建 TurnTask 管理**（shared_runtime.rs）：新增注册表与 spawn/send_command/
   is_running/cancel_and_join，与 inbox 并存不切换。
2. **deliver 用户消息改为"空闲起轮、运行中投通道"**：空闲起轮（build ctx →
   保存 → spawn_turn）、运行中 send_command 引导；run_user_turn 逻辑改为
   直启闭包。
3. **空闲期命令回归 Core**：压缩、清空、标题、信任模式、思考强度改为 Core 方法
   双分支协调（运行中转发、空闲直写）。
4. **状态查询切换**：is_stopped/is_busy 改查 TurnTask 注册表。
5. **关闭路径重写**：shutdown → cancel_and_join → 未处理消息落盘；
   CoreManager（core_manager/mod.rs:313）与 Drop（detach）对齐新接口。
6. **删除 inbox 调度层**：删除第 4 节清单；CommandIngress 迁居。
7. **测试迁移**：wait_idle / is_running / 投递顺序相关 helper 与
   contract/integration 测试适配新模型；删除 Driver 专属契约
   （如 running 标志的三个语义测试）。
8. **文档同步**：重写 design.md（统一通道章节作废，记录回归原因）；
   修订 PLAN.md 里程碑 B 第 2、3 条。

## 7. 风险与影响

- **直启与收尾窗口并发写 session**：turn 收尾（落盘、插件回调）与新消息直启
  可能交错。依赖 spawn_turn 双检兜底 + 收尾窗口毫秒级，与主分支在线行为一致。
- **测试重写量**：integration/contract 测试中依赖通道排队语义的用例需要改造，
  估计占现有测试两成以内；turn 内部行为测试（execute.rs 系列）不受影响。
- **插件后台任务反馈**：turn 结束后迟到的反馈由封口机制拒绝，行为不变，
  但封口宿主从 CommandIngress 迁移时需回归相关测试。
- **agent-team 子 Agent**：子 Core 释放后任务在共享 runtime 上自然跑完落盘，
  与现状一致，无新增风险。
- **回退不可逆点**：步骤 6 删除 inbox 后，若发现遗漏依赖需要从 git 历史找回。

## 8. 规模估计

约 6–8 个提交粒度；其中步骤 2、5 是行为关键路径，需重点测试。
