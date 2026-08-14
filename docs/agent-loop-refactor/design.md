# Agent 执行核心重构 — 总体设计

> 需求契约：[requirements.md](./requirements.md)
>
> 执行进度：[progress.md](./progress.md)

## 1. 目标架构

最终执行核心由四个边界组成：

```text
CommandIngress
  └─ 可靠接收、封口、下一 turn 交接

ExecutionDriver（唯一事件循环）
  ├─ ExecutionState（单一阶段 + 预算 + 用量）
  ├─ CommandHandler
  └─ Phase Drivers
       ├─ Model
       ├─ Tools / Approval
       ├─ Compression
       └─ CompletionPolicy

TurnCommitter
  └─ 协议修复、状态锚点、插件收尾、最终保存、唯一终态
```

核心原则：

- **阶段唯一**：执行活动资源由当前阶段持有。
- **事件统一**：命令和异步结果都先转成事件，再产生阶段迁移。
- **执行与提交分离**：`PendingFinish` 是执行阶段；`Committing` 是入口封口后的 turn 提交状态。
- **行为契约先行**：当前缺陷不因“行为不变”被固化。
- **小步替换**：每个阶段独立迁移和验证。

## 2. 领域模型

### 2.1 执行状态

```rust
struct ExecutionState {
    phase: ExecutionPhase,
    budget: ExecutionBudget,
    limits: ExecutionLimits,
    accumulated_usage: TokenUsage,
    tool_history: ToolCallHistory,
    injections: ToolInjectionBuffer,
    trust_mode: TrustMode,
    intent_generation: u64,
}
```

### 2.2 执行预算

```rust
struct ExecutionBudget {
    request_round: usize,
    react_rounds_in_phase: usize,
    continuation_count: u32,
    executed_tool_in_phase: bool,
}

struct ExecutionLimits {
    max_react_rounds: usize,
    max_continuation_checks: u32,
}
```

新用户意图调用 `reset_for_new_intent()`：

- 重置阶段轮数、续作次数、工具执行标记和工具去重历史；
- 保留 `request_round` 作为物理 turn 内日志序号；
- 保留 `accumulated_usage`。

### 2.3 阶段定义

```rust
enum ExecutionPhase {
    NeedModel,
    WaitingModel(ActiveLlm),
    PreparingTools(ToolBatchState),
    WaitingTools(ToolExecutionPhase),
    WaitingApproval(ApprovalPhase),
    Compressing(CompressionPhase),
    CheckingCompletion(ActiveLlm),
    ForceFinal(ActiveLlm),
    PendingFinish(PendingResult),
}

struct ToolExecutionPhase {
    tasks: JoinSet<ToolTaskOutput>,
    running: HashMap<TaskId, RunningToolCall>,
    batch: ToolBatchState,
}

struct ApprovalPhase {
    pending: PendingApproval,
    batch: ToolBatchState,
}

struct CompressionPhase {
    active: ActiveCompression<CompressionContinuation>,
    return_to: CompressionReturn,
}
```

`WaitingApproval` 必须持有完整工具批次，避免审批完成后丢失尚未处理的工具和批次元数据。

### 2.4 执行事件

```rust
enum ExecutionEvent {
    Command(Command),
    ModelChunk(ModelStreamChunk),
    ModelCompleted(ModelCompletion),
    ToolCompleted(ToolCompletion),
    CompressionCompleted(CompressionResult),
    Advance,
}
```

事件处理产生：

```rust
enum Transition {
    Stay,
    To(ExecutionPhase),
    Finish(PendingResult),
    Cancel,
    Fail(String),
}
```

## 3. Rust 驱动骨架

状态机采用“阶段所有权取出 → 等待一个事件 → 生成新阶段”的方式，避免同时借用 `state.phase` 与整个 `state`。

概念流程：

```rust
loop {
    let phase = state.take_phase()?;
    let (next_phase, effect) = drive_phase(phase, &mut state, cmd_rx, &ctx).await?;
    state.install_phase(next_phase)?;
    apply_effect(effect, &mut state, ctx).await?;
}
```

实现要求：

1. 不为了绕过借用问题重新引入并列活动 `Option`。
2. 取出阶段后，所有退出路径都必须安装新阶段或形成最终结果。
3. 如使用临时占位，使用内部不可对外观察的 `Transitioning`/`Option<ExecutionPhase>`，并以守卫保证 panic/错误路径可诊断；稳定完成后不得让“无阶段”成为业务状态。
4. Ready 阶段直接同步推进；Waiting 阶段才进入 `tokio::select!`。

不提供含义模糊的 `is_idle()`。阶段分为：

- **Ready**：`NeedModel`、`PreparingTools`、`PendingFinish`；
- **Waiting**：`WaitingModel`、`WaitingTools`、`WaitingApproval`、`Compressing`、`CheckingCompletion`、`ForceFinal`。

### 3.1 任务 02 原型验证结论

`react/phase.rs` 的最小原型（`ProtoState` / `ProtoPhase` / `drive_phase` / `proto_drive_loop`）已验证（3 个测试通过）：

- **take/install 模式可行**：`take_phase` 取出阶段（`Option::take`），`install_phase` 安装新阶段，中间"无阶段"窗口由断言约束（ALR-205），循环中无需并列活动 `Option`。
- **活动资源可整体转移**：阶段持有 `JoinHandle`/`JoinSet` 时，迁移通过消费旧阶段、构造新阶段完成，无需在 state 上维护并列 `Option`。
- **取消用 `AbortHandle`**：`WaitingModel` 的 `tokio::select!` 消费 `JoinHandle`，取消改用 `handle.abort_handle()` 产生的独立 `AbortHandle`，避免与 select 消费 handle 冲突。
- **Ready/Waiting 分流**：Ready 阶段同步推进（构造新阶段返回），Waiting 阶段进入 `select!`，不引入含义模糊的 `is_idle`。

后续任务（03 起）按此骨架扩展为正式 `ExecutionPhase` / `ExecutionState`，无需另选所有权模型。

## 4. 阶段迁移

```text
NeedModel
  → WaitingModel

WaitingModel
  ├─ 工具调用          → PreparingTools
  ├─ 直接完整答复      → PendingFinish
  ├─ 需完成度检查      → CheckingCompletion
  ├─ 上下文超限        → Compressing(return_to=NeedModel)
  └─ 用户引导          → 中断并保存 → NeedModel

PreparingTools
  ├─ 可直接执行        → WaitingTools
  ├─ 需要审批          → WaitingApproval
  └─ 批次处理完成      → NeedModel

WaitingTools
  ├─ 单项完成          → WaitingTools / PreparingTools
  ├─ 整批完成          → NeedModel
  ├─ 用户引导          → 闭合协议 → NeedModel
  └─ Cancel            → PendingFinish(Cancelled)

WaitingApproval
  ├─ 批准              → PreparingTools / WaitingTools
  ├─ 拒绝              → NeedModel / PendingFinish
  ├─ FullTrust         → PreparingTools
  └─ 用户引导          → 闭合审批工具 → NeedModel

Compressing
  ├─ 完成              → return_to 对应阶段
  ├─ 失败              → 重试或 PendingFinish(Failed)
  └─ 用户引导          → 取消压缩 → NeedModel

CheckingCompletion
  ├─ Done / AskUser    → PendingFinish
  ├─ NeedMoreWork      → NeedModel（continuation_count += 1）
  └─ 超限              → ForceFinal

ForceFinal
  ├─ 成功              → PendingFinish
  └─ 失败              → PendingFinish(Failed)

PendingFinish
  ├─ 用户引导          → NeedModel（重置新意图预算）
  ├─ InjectTool        → NeedModel（不重置用户意图预算）
  ├─ 有效审批          → 对应工具阶段
  ├─ Cancel / Shutdown → PendingFinish(Cancelled)
  ├─ 标题/配置/用量    → 执行副作用并保持
  └─ ingress 封口成功  → TurnCommitter
```

## 5. 命令与阶段语义

所有命令只经过一个 `CommandHandler`。处理结果不直接散落修改多个活动状态，而是返回 `CommandEffect`：

```rust
enum CommandEffect {
    Continue,
    RestartForUserIntent,
    RestartForToolInjection,
    ResumeTools,
    ReplacePending(PendingResult),
    Cancel,
    Shutdown,
}
```

最低要求：

| 命令 | Waiting 阶段 | PendingFinish |
| --- | --- | --- |
| InjectUserMessage | 中断、落盘、重置预算、NeedModel | 撤销结果、落盘、重置预算、NeedModel |
| InjectTool | 注入代际处理 | 注入并重新分析 |
| Cancel / Shutdown | 停止主循环活动 | 替换暂定结果并提交取消 |
| SetTrustMode | 更新；可能恢复审批 | 更新后保留或恢复工具 |
| SetReasoningEffort | 更新下一请求配置 | 更新后保持暂定结果 |
| SetTitle | 更新标题 | 更新标题后保持 |
| ReportUsage | 累计最新用量 | 累计后最终结果必须读取最新用量 |
| EmitStreamEvent | 转发 | 转发后保持 |
| Approval | 按阶段校验 | 有效则恢复工具；迟到则明确忽略 |

## 6. 终态入口门控

### 6.1 状态

```rust
enum IngressState {
    Accepting,
    Sealing,
    Committing,
}
```

### 6.2 语义

- `Accepting`：命令可以进入当前 turn。
- `Sealing`：暂停新命令进入旧队列，处理封口前已接受命令。
- `Committing`：旧 turn 不再接收命令，用户消息可靠进入下一 turn 队列或返回明确交接结果。

所有命令来源必须经过同一个 `CommandIngress`：

- `TiangongCore::deliver`；
- `shared_runtime::send_command`；
- `PluginFeedbackTx`；
- 标题生成回传；
- 插件用量和流事件；
- 工具/浏览器/终端注入。

### 6.3 封口流程

```text
PendingFinish
  → 原子 Accepting → Sealing
  → 排空封口前已接受命令
      ├─ 命令要求继续：Sealing → Accepting，迁回执行阶段
      └─ 无继续命令：Sealing → Committing
  → TurnCommitter
```

用户消息确认结果：

```rust
enum MessageDeliveryResult {
    SavedInCurrentTurn,
    QueuedForNextTurn,
    Rejected(String),
}
```

禁止把“写入 mpsc 成功”当作消息处理成功。

## 7. 迟到结果与结构化并发

不预设所有异步结果都靠 `intent_generation` 丢弃。采用分层策略：

1. **首选结构化取消**：任务由阶段持有，中断时取消并等待；阶段资源消失后结果不能再推进。
2. **意图代际作为补充**：仅对无法通过所有权和 join 保证的回传通道使用。
3. **结果分类处理**：
   - 模型旧结果：不得写入新阶段；实际用量按已观测数据处理。
   - 工具旧结果：不得推进新阶段，但必须考虑真实副作用、协议闭合和审计。
   - 压缩旧结果：不得应用旧摘要；已消耗用量和取消事件按契约处理。
4. 必须先有可复现迟到路径或明确通道，再增加代际字段。

## 8. 完成度策略

第一阶段保持现有 Summary/ForceFinal 用户行为，但从驱动循环中解耦：

```rust
trait CompletionPolicy {
    fn decide_next(...) -> CompletionAction;
}
```

完成度策略输出：

- `Finish`；
- `AskUser`；
- `Continue`；
- `ForceFinal`。

结构稳定后，基于指标评估按需 Summary：

- 直接完整回答跳过 Summary；
- 明显完整的工具后答复跳过 Summary；
- 仅完成度不明确、达到预算或输出异常时调用独立检查。

该优化必须具备对照测试、日志指标和回滚开关，不在状态机迁移任务中顺手改变。

## 9. 模块边界

目标目录：

```text
react/
  execute.rs        # ExecutionDriver 与事件循环
  phase.rs          # 阶段、状态、预算、迁移结果
  command.rs        # 统一命令处理
  model_phase.rs    # 模型请求与响应归一化
  tool_phase.rs     # 工具批次、并行工具、审批
  compression.rs    # 压缩阶段（现有模块演进）
  completion.rs     # Summary/ForceFinal 策略
  interrupt.rs      # 引导/取消的结构化中断与协议闭合
  ingress.rs        # 当前 turn 接收门控和下一 turn 交接
  turn.rs           # 生命周期与唯一终态提交
```

拆分顺序遵循“先稳定逻辑，后机械移动”。

## 10. 可观测性

关键日志至少包含：

- session_id；
- intent_generation（如使用）；
- from_phase / event / to_phase；
- budget 快照；
- pending result 类型；
- ingress 状态迁移；
- 迟到结果处置原因。

日志不得记录敏感正文或附件数据。

## 11. 兼容性与迁移策略

- Session 落盘结构默认不变。
- StreamEvent 外部契约默认不变；如终态交接确需新增内部事件，先证明不能用内部确认通道实现。
- 插件后台任务语义不变。
- 状态机迁移期间允许短期双轨，但每个双轨任务必须明确权威状态和删除点，不能跨多个任务长期并存。
- 每个任务独立提交、验证和审查；失败时回滚到上一个绿色提交。

## 12. 需求追踪

设计覆盖声明：

- 架构：ALR-001、ALR-002、ALR-003、ALR-004、ALR-005。
- 行为：ALR-101、ALR-102、ALR-103、ALR-104、ALR-105、ALR-106、ALR-107、ALR-108、ALR-109、ALR-110、ALR-111。
- 并发可靠性：ALR-201、ALR-202、ALR-203、ALR-204、ALR-205。
- 可观测与演进：ALR-301、ALR-302、ALR-303、ALR-304。

| 需求 | 设计落点 | 任务 | 验证 |
| --- | --- | --- | --- |
| ALR-001~005 | 阶段/驱动/模块 | 02~07 | 类型与阶段测试、clippy、workspace check |
| ALR-101~104 | 中断/预算/模型阶段 | 01/03/04/07 | 引导与 Summary 中断测试 |
| ALR-105~106 | PendingFinish/命令 | 01/07 | 命令矩阵与顺序测试 |
| ALR-107~110 | TurnCommitter/工具协议 | 01/06/09 | 锚点、生命周期、协议测试 |
| ALR-111 | PendingResult/封口 | 01/07/09 | 晚到用量测试 |
| ALR-201~203 | ingress | 08 | 并发封口与下一 turn 交接测试 |
| ALR-204~205 | 并发策略/迁移守卫 | 02/09 | 迟到结果与迁移失败测试 |
| ALR-301~304 | 日志/策略/任务治理 | 09/10 | 日志断言、性能对照、审查记录 |

## 13. 最终验收

- 单阶段、单事件循环、统一预算和统一命令处理落地。
- 原子终态封口和可靠下一 turn 交接通过并发测试。
- 关键行为、事件顺序、生命周期、协议和用量自动化测试通过。
- Summary 策略变更如启用，具备指标与回滚开关。
- 完整检查通过，且文档、PLAN、TODO、进度记录同步。
