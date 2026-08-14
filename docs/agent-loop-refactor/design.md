# Agent 执行核心重构 — 简化收敛设计

> 需求契约：[requirements.md](./requirements.md)
>
> 执行进度：[progress.md](./progress.md)
>
> 本设计参考 DeepSeek Harness 的 Agent Loop，但只借鉴职责边界和并发模型，不照搬其 TypeScript 结构、事件命名或仅追加 Session 格式。

## 1. 为什么需要再次收敛

当前实现已经完成单阶段迁移，却仍保留旧控制模型：

- 独立 Summary 判断是否完成；
- `NeedMoreWork` 后续作；
- 达到次数后 ForceFinal；
- 审批和压缩作为 Loop 顶层阶段；
- 封口后用临时队列与一次性后台任务自动启动下一 turn。

这使“状态统一”没有转化为“职责减少”。尤其是下一 turn 交接，补偿分支已经出现旧 Session 快照、重复确认、后台失败后无人调度、抢占重复启动和关闭时静默清队列等问题，达到原设计规定的“复杂度失控后回退并重新评审 ALR-202”条件。

因此不再继续修补现有交接方案，而是把 Loop、Inbox、请求策略和工具流水线重新划界。

## 2. DeepSeek Harness 的可借鉴部分

DeepSeek Harness 的核心做法可以概括为：

1. `step()` 调模型并记录响应；有工具调用就执行工具，无工具调用就形成自然停止候选。Harness 随后可完成 turn，但天工还需要检查本地可判定的工具义务。
2. `turn()` 在 turn 边界领取 followup，在 step 边界领取 steer/inject；Inbox 未空时由同一个 driver 继续运行。
3. Agent 对外只有 `idle` / `running`，取消收敛窗口使用 `wakeRequested` 锁存唤醒。
4. Session 是唯一真源，每次请求从最新日志派生上下文。
5. 压缩通过请求前和请求错误扩展点接入。
6. 审批属于工具执行服务，和工具共享取消信号。
7. 重试由请求错误扩展点决定。
8. 工具调度器独立处理并行、屏障、顺序提交和取消。
9. Loop 不内置 Summary、ForceFinal、外层续作次数或轮次预算。

### 2.1 天工可直接借鉴

- 最小模型—工具循环；
- `Idle | Running` 外部状态；
- Agent 自有 `next_turn` / `next_step` Inbox；
- 单 driver 持续排空 Inbox；
- `wake_requested` 处理取消到 idle 的竞态窗口；
- 压缩、审批、重试和预算外置；
- 测试公开行为而非内部阶段。

### 2.2 天工只能等价实现

| Harness 做法 | 天工约束 | 等价实现 |
| --- | --- | --- |
| 仅追加事件日志 Session | 当前 Session 落盘格式和消息更新方式已形成兼容契约 | 保持格式不变，但所有新 turn 都在启动时读取最新 Session，禁止未来上下文快照 |
| TypeScript 单线程对象状态 | Rust 异步任务、所有权和跨线程命令入口 | driver 独占执行状态；Inbox 使用短锁；活动任务用取消令牌和 join 收敛 |
| 插件化 pre-step/request-error | 天工已有插件生命周期，不宜一次重写公开接口 | 先建立 core 内部请求策略接口，再按需要映射到既有插件扩展点 |
| approval 子系统 | 天工已有审批命令和信任模式 | 保持对外命令，内部把等待审批封装进工具执行器，不提升为 Agent 顶层阶段 |
| Inbox 事件命名 | 天工已有 InjectUserMessage/InjectTool 等命令 | 在 ingress 层显式映射为 followup/steer/inject，逐步收敛内部命名 |
| 无内置预算 | 天工需要防止失控调用 | 将模型数、耗时和费用限制做成可选安全策略，由它取消活动，而不是改变 Loop 完成语义 |
| 无工具即停止 | 天工真实场景中模型可能漏发必需 tool call，例如只描述“已执行”却没有命令或文件结果 | 将无工具响应视为候选完成；用结构化 `TaskContract` 检查工具义务，优先使用 Provider `tool_choice` 约束，违约时有界修复或明确失败 |

### 2.3 天工早期单循环实现对照

这里的“早期单循环”不是推测，而是仓库历史中的真实实现。关键节点如下：

| 历史节点 | 时间 | 实现特征 | 无 tool call 的处理 |
| --- | --- | --- | --- |
| `db18a318` | 2026-05-09 | 消除 sync/async 双版本后，`execute_turn` 由单个 `'react_loop` 驱动；当时 `engine.rs` 约 614 行 | 除合成占位符外，直接保存 assistant 文本、发送 `Done` 并返回 |
| `ca9e465b` | 2026-05-19 | 在单循环里加入第一次漏工具启发式修复 | 只有“存在 reasoning 且正文不超过 100 字”时追加 reminder 并重试，其他纯文本仍直接完成 |
| `06c0ac18` | 2026-05-20 | 用 lite 模型替代简易规则，仍保持单循环 | 最多两次判断；判定未完成时保存文本、注入“需要操作就返回 tool_calls”并继续；次数耗尽后仍接受纯文本 |
| `474389d0^` | 2026-06-26 前 | 功能扩展后的成熟单循环，`engine.rs` 约 2428 行 | 保留 lite 完成判断和最多两次重入 |
| `474389d0` | 2026-06-26 | 引入 ReAct + Summary 双阶段和外层循环 | ReAct 无工具后进入 Summary，由主模型判断完成、提问或 `NEED_MORE_WORK` |

`474389d0` 将 `engine.rs` 从约 2428 行增至约 2853 行，该文件单次变更新增 1399 行、删除 974 行。它解决的是早期单循环中“模型漏发工具却直接结束”的真实问题，但代价是引入 Summary、outer iteration 和 ForceFinal，后来这些概念一直延续到当前过渡实现。

#### 2.3.1 早期单循环的真实骨架

以 `db18a318` 为例，主干可以概括为：

```text
'react_loop:
  排空当前 turn 命令
  → 达到 max_rounds：force_final_response
  → 从当前 Session + loop_context 组装请求
  → 调模型，同时监听命令和流式输出
  → 无 tool_calls：保存 assistant → Done
  → 有 tool_calls：权限/审批 → 执行 → 保存结果 → continue
```

它已经具备一些值得保留的特征：

- 只有一个明确的模型—工具控制循环；
- 工具结果直接成为下一轮模型上下文；
- 同一 `execute_turn` 持有当前 Session，不为未来 turn 构造快照；
- 命令在请求前排空，并在模型流式请求期间通过 `select` 接收；
- 没有 Summary、outer iteration 或 continuation count。

但“单循环”不等于“职责简单”。权限审批、命令处理、上下文压缩、工具调度、Session 保存、Memory 和终态发布仍全部内联在 `engine.rs`；功能增加后，在引入双阶段前文件已经增长到约 2428 行。

#### 2.3.2 早期方案为什么没有解决漏工具

三代处理都有明确缺陷：

1. **直接完成**：`db18a318` 把无工具纯文本直接视为成功，正好对应当前实际问题。
2. **文本启发式**：`ca9e465b` 只覆盖“有 reasoning + 短回复”，较长的解释性文本、虚假的“已执行”说明和没有 reasoning 的响应都会漏过。
3. **lite 模型判断**：`06c0ac18` 覆盖面更大，但仍让另一个概率模型判断是否完成；最多两次后会放行，而且它没有命令执行结果、附件读取记录或验证成功记录这样的确定性证据。

后来的双阶段 Summary 把判断者从 lite 模型换成主模型并增加重入，但本质仍是“让模型判断另一个模型是否完成”。它改善了表现，却没有建立“必须获得真实工具结果”的硬边界。

#### 2.3.3 新方案与早期单循环的关系

新方案不是简单回退到旧代码，而是“恢复单循环控制面，保留后来积累的可靠性，再替换旧完成判断”：

| 维度 | 早期单循环 | 双阶段/当前过渡实现 | 新简化方案 |
| --- | --- | --- | --- |
| 主控制流 | 单个 ReAct loop | ReAct + Summary + ForceFinal，当前映射为多个阶段 | 单个 turn/step driver |
| 无工具响应 | 直接完成，后期由启发式/lite 模型补救 | Summary/CompletionPolicy 再判断 | 只形成候选完成 |
| 工具必需性 | 没有结构化证据 | 由模型语义判断 | `TaskContract` 确定性义务 |
| 漏工具修复 | 提示词重试，范围不稳定 | Summary `NeedMoreWork` 重入 | Provider `tool_choice` + 有界协议修复 |
| 修复耗尽 | 放行文本或 max rounds 强制回复 | ForceFinal | 明确失败，不虚报成功 |
| 工具/审批/压缩 | 全部内联在 Loop | 提升为顶层阶段 | 工具流水线/请求策略 |
| 连续输入 | 当前 turn 命令通道，缺少跨 turn 持久 Inbox | 封口队列 + 每消息后台任务 | Agent Inbox + 唯一 driver |
| Session | 当前 turn 直接持有 | 下一 turn 方案可能捕获旧快照 | turn 启动时读取最新 Session |
| 顶层状态 | 隐式 running | 多个执行阶段 | `Idle | Running` |

因此应保留早期版本的“一个 driver 直接推进模型和工具”，但不能恢复“无工具即成功”、文本长度启发式、lite 完成模型、内联所有子系统或 max rounds 后强制伪造最终回答。

## 3. 目标结构

```text
Core / API / PluginFeedback
          │
          ▼
     AgentIngress
       ├─ followup ───────────────┐
       ├─ steer ───────────────┐  │
       └─ inject ────────────┐ │  │
                             ▼ ▼  ▼
                       AgentInbox
                   ├─ next_turn: FIFO
                   └─ next_step: ordered
                             │
                             ▼
                    AgentDriver (唯一)
                   state: Idle | Running
                             │
               ┌─────────────┴─────────────┐
               ▼                           ▼
           TurnDriver                  wake_requested
               │
               ▼
            StepDriver
      pre-step → model → record
               │
        ┌──────┴──────┐
        ▼             ▼
    tool calls      no tool calls
        │             │
        ▼             ▼
 ToolExecutionPipeline   CompletionGate
 approval / schedule     ├─ obligations satisfied → stopping/完成
 / cancel                └─ missing obligations → ToolProtocolRepair
        │                                      │
        └──────────────→ 下一 step ←───────────┘

TaskContract / ModelRequestPolicy
  ├─ required tool obligations / tool_choice
  ├─ context preparation / compaction
  ├─ request retry / bounded protocol repair
  └─ external safety limits
```

### 3.1 顶层状态

```rust
enum AgentRunState {
    Idle,
    Running {
        cancel: CancellationToken,
    },
}

struct AgentRuntime {
    state: AgentRunState,
    inbox: AgentInbox,
    wake_requested: bool,
}
```

这是 Agent 的调度状态，不要求把全部字段放在一个锁内。实现时应满足：

- 状态切换和唤醒判定在一个短临界区内完成；
- driver 不在持锁状态执行模型、工具、Session I/O 或插件回调；
- mutable turn/step 状态只由 driver 任务持有；
- 任何时刻最多有一个 driver 获得 `Running` 所有权。

模型请求、工具批次和审批等待是 driver 内部局部活动，不是新的 Agent 顶层状态。

## 4. Inbox 设计

```rust
struct AgentInbox {
    next_turn: VecDeque<TurnInput>,
    next_step: VecDeque<StepInput>,
    accepting: bool,
}

enum TurnInput {
    UserMessage(UserInput),
}

enum StepInput {
    Steer(UserInput),
    Inject(ToolOrPluginInput),
}
```

### 4.1 三种输入语义

| 输入 | 队列 | 是否请求唤醒 | 语义 |
| --- | --- | --- | --- |
| followup | `next_turn` | 是 | 当前逻辑 turn 完成后开始一个新 turn |
| steer | `next_step` | 是 | 修正当前意图；取消当前 Loop 直接拥有的活动，在下一 step 生效 |
| inject | `next_step` | 否 | 补充工具/插件结果；等待当前活动自然到达下一 step |

`steer` 的“唤醒”不等于无条件新建任务：

- `Idle`：尝试启动唯一 driver；
- `Running`：通知当前活动取消/尽快到达 step 边界；
- 正在取消收敛：设置 `wake_requested`，待 driver 进入 idle 后重放。

### 4.2 领取规则

1. driver 从 `Idle` 成功切为 `Running` 后开始排空。
2. turn 开始时：
   - FIFO 领取一条 `next_turn`；
   - 领取此刻已有的全部 `next_step`；
   - 从最新 Session 创建 TurnContext；
   - 调用一次 `on_turn_started`。
3. 每个 step 开始前领取新的 `next_step`，按原顺序写入/合并上下文。
4. 模型无工具调用后进入 stopping 检查：
   - 有 `next_step`：继续下一 step；
   - 无 `next_step`：提交 turn，并调用一次 `on_turn_finished`。
5. turn 完成后：
   - 有 `next_turn`：同一 driver 开始下一 turn；
   - 无输入：尝试切回 `Idle`。
6. 切回 `Idle` 的同一临界区检查 `wake_requested` 和 Inbox：只要任一成立，就重新获得 `Running` 并继续，不创建第二个 driver。

### 4.3 可靠确认边界

消息入队成功不是充分条件；还必须确认该队列由活跃 Agent 的唯一 driver 负责，并且 Agent 未进入关闭状态。

```rust
enum DeliveryResult {
    AcceptedCurrentStep,
    QueuedNextTurn,
    Rejected(DeliveryError),
}
```

规则：

- `accepting == false` 时直接拒绝；
- 接受和触发/锁存唤醒属于同一调度临界区；
- 不返回“后台可能启动”的模糊成功；
- Core 关闭先停止接收，再让 driver 收敛；不能静默 `clear()` 已确认输入；
- 当前范围的可靠性是 Agent 实例生命周期内可靠，不声称内存队列能抵抗进程崩溃。

## 5. Driver 与最小 Loop

概念伪代码：

```rust
async fn drive_agent(agent: Arc<Agent>) {
    loop {
        let Some(turn_input) = agent.inbox.take_next_turn_or_resumable_input() else {
            if agent.try_become_idle_or_reacquire_wake() {
                continue;
            }
            return;
        };

        let mut turn = TurnContext::from_latest_session(turn_input).await?;
        turn.start_once().await?;

        let result = drive_turn(agent.clone(), &mut turn).await;
        turn.finish_once(result).await?;
    }
}

async fn drive_turn(agent: Arc<Agent>, turn: &mut TurnContext) -> TurnResult {
    loop {
        let inputs = agent.inbox.take_next_step();
        turn.apply(inputs).await?;

        let response = request_policy.call_model(turn).await?;
        turn.record(response.clone()).await?;

        if response.tool_calls.is_empty() {
            if agent.inbox.has_next_step() {
                continue;
            }

            match completion_gate.check(turn.task_contract(), &response) {
                CompletionDecision::Complete => return TurnResult::Completed(response),
                CompletionDecision::Repair(missing) => {
                    request_policy.require_tools(missing)?;
                    continue;
                }
                CompletionDecision::Fail(reason) => return TurnResult::Failed(reason),
            }
        }

        let results = tool_pipeline
            .execute(response.tool_calls, turn.cancel_token())
            .await?;
        turn.task_contract_mut().apply_tool_results(&results)?;
    }
}
```

伪代码只表达职责，不规定最终 API。实现必须额外处理流式消息、Provider 协议、用量和插件事件，但不能重新引入 Summary/ForceFinal 完成循环。

### 5.1 候选完成与 stopping 检查

“模型无工具调用”只表示模型希望停止，不能单独证明任务已经完成。提交前依次执行：

1. 领取竞态到达的 `next_step`；有输入则继续。
2. 用确定性的 `CompletionGate` 检查 `TaskContract` 中是否仍有未满足工具义务。
3. 无义务才提交；有义务则进入工具协议修复；修复预算耗尽则明确失败。

`CompletionGate` 不调用第二个自由文本模型，也不判断“回答看起来是否完整”。它只检查程序可验证的事实，例如用户是否明确要求执行命令、指定附件是否实际读取、代码修改后验证命令是否实际成功、声明依赖的工具结果是否已经保存。

### 5.2 取消语义

- steer：取消 Loop 直接拥有的模型请求或工具等待，收敛协议后进入下一 step；不默认调用插件 `on_cancel`。
- cancel turn：取消当前模型、工具流水线和需要随 turn 结束的插件活动，形成唯一取消终态。
- shutdown：停止接收新输入，取消或排空按 Core 关闭契约处理，最终不得遗留已确认但无人负责的 Inbox 项。

## 6. 工具义务与协议修复

### 6.1 `TaskContract` 的职责

`TaskContract` 不是旧 Summary 的改名版本。它不理解开放式语义，也不调用 LLM；它记录入口或执行过程已经明确产生、可以由程序核验的义务：

```rust
struct TaskContract {
    obligations: Vec<ToolObligation>,
    protocol_repairs: u8,
    max_tool_protocol_repairs: u8,
}

enum ToolObligation {
    ReadAttachment { attachment_id: String },
    ExecuteCommand { request_id: String },
    ModifyWorkspace { scope: PathScope },
    ValidateWorkspace { check: ValidationKind },
    ObtainToolResult { capability: ToolCapability },
}
```

义务来源按可靠性从高到低分层：

1. API/入口显式声明，例如附件列表、命令执行请求、文件编辑请求；
2. 系统和项目规则，例如“代码修改完成后必须验证”；
3. 已执行动作产生的后置条件，例如写入代码后增加验证义务；
4. 用户自然语言中的高置信度动作，由现有命令分类或轻量规则映射；不确定时不能凭空强制某个具体工具。

普通解释、写作、翻译和闲聊没有工具义务，模型纯文本可以直接完成。需要外部事实但没有合适工具时，应明确说明限制，而不是伪造结果。

### 6.2 三层防线

#### 第一层：请求前约束

- 当前存在明确工具义务且 Provider 支持时，设置 `tool_choice=required`；
- 若义务唯一对应一个工具，可指定该工具；
- 将缺失义务以结构化、短文本形式放入请求，不依赖冗长提示词；
- 不能在已有合法 tool call 的同一响应上额外制造虚拟调用。

#### 第二层：候选完成门控

模型返回纯文本时，`CompletionGate` 检查：

- 是否还有未消费的 `next_step`；
- 是否存在未满足的必需工具义务；
- 工具结果是否已成功持久化；
- 需要验证的改动是否真的获得成功验证结果。

门控通过才发布成功终态。模型文字中声称“已经执行”“已经读取”不构成证据。

#### 第三层：有界协议修复

如果模型漏发 tool call：

1. 不把该纯文本提交为最终答复；
2. 记录 `missing_required_tool_call` 诊断事件；
3. 下一请求明确列出仍缺少的义务，并使用 Provider 原生工具约束；
4. 修复成功后正常执行工具并回到最小 Loop；
5. 达到 `max_tool_protocol_repairs` 后明确失败，不进入无限续作。

建议默认上限为 1～2 次，由 Provider 能力和实际指标确定。这个计数只保护“模型违反工具协议”的异常路径，不参与一般完成度判断，因此不会恢复旧 outer loop。

### 6.3 不应强制工具的场景

- 用户只问概念解释；
- 已有 Session 证据足够回答，且没有时效性要求；
- 工具义务已经由成功结果满足；
- 用户明确要求只给建议、不实际操作；
- 工具不可用且继续请求也不能改变结果。

这样既避免“该用工具却直接回答”，也避免所有请求都强制调用工具。

## 7. Session 与上下文

天工本轮不改造 Session 格式，但采用以下约束获得 Harness“Session 唯一真源”的核心收益：

1. 用户输入先经唯一入口保存或进入 Agent Inbox。
2. turn 真正开始后才读取最新 Session 并构建上下文。
3. 前一 turn 运行期间不得为下一 turn创建 `TurnContext`。
4. 模型响应、工具结果、消息状态和用量仍通过单一权威保存路径更新。
5. turn 提交完成后，下一 turn 才能看到前一 turn 的最终数据。
6. 如果保存失败，不能把对应输入视为已消费成功；按现有错误契约重试或明确失败。

这避免当前“一次性后台任务捕获旧 Session，然后覆盖刚完成 turn 数据”的风险。

## 8. 工具执行流水线

```text
model tool calls
      │
      ▼
normalize & validate
      │
      ▼
approval service ── cancelled/rejected → protocol-safe result
      │
      ▼
scheduler
  ├─ bounded parallel pool
  ├─ exclusive tool barrier
  └─ ordered result commit
      │
      ▼
Session / next model step
```

工具流水线负责：

- 参数校验与重复调用规则；
- 权限判断、审批等待和信任模式变化；
- 并行工具的有界滚动执行；
- 独占工具前后的屏障；
- 所有任务共享 turn/step 取消信号；
- 真实副作用审计；
- 按模型声明顺序提交结果；
- 中断时为每个已发出的 Provider tool call 生成合法结果或按 Provider 契约安全移除。

Loop 只等待流水线完成，不需要 `PreparingTools`、`WaitingTools`、`WaitingApproval` 三个顶层阶段来表达 Agent 生命周期。

## 9. 模型请求策略

```rust
enum RequestDecision<T> {
    Ready(T),
    Retry,
    Fail(RequestError),
}
```

请求策略分四层：

### 9.1 请求前

- 读取最新上下文；
- 估算上下文压力；
- 必要时执行压缩；
- 应用模型配置和推理强度；
- 执行可选安全预算检查。

### 9.2 请求错误

- Provider 临时错误：按策略退避重试；
- 上下文溢出：压缩后返回 `Retry`；
- 不可恢复错误：返回 `Fail`；
- 取消：直接传播，不伪装为请求失败。

### 9.3 工具协议修复

- `TaskContract` 存在未满足义务时，为请求附加缺失义务；
- Provider 支持时设置 `tool_choice=required` 或指定工具；
- 每次修复递增独立计数，并记录模型、Provider、义务类型和响应形态；
- 超过小上限后返回可诊断失败，不能降级为未经验证的成功文本。

### 9.4 外部安全预算

如果产品仍需限制失控执行，策略可观察：

- 模型请求数；
- 总耗时；
- token/费用；
- 连续工具调用数。

达到阈值时策略取消或失败当前 turn。它不生成 Summary，不强迫模型再输出一次，也不成为“外层循环”。

## 10. 删除与模块收敛

### 10.1 删除对象

- `completion_policy.rs` 中仅服务 Summary/ForceFinal 的策略；
- `StartCheckingCompletion` / `CheckingCompletion`；
- `StartForceFinal` / `ForceFinalPhase`；
- `continuation_count` 和 `max_outer_iterations`；
- Summary/ForceFinal 的独立模型请求与补偿分支；
- 当前 `NEXT_TURN_QUEUE`；
- 每条排队消息对应的一次性 spawn；
- 未来 turn 的提前 Session/TurnContext 快照。

### 10.2 目标模块职责

```text
react/
  execute.rs          # 唯一 AgentDriver / TurnDriver / StepDriver 编排
  inbox.rs            # next_turn / next_step / wake_requested / delivery
  request.rs          # 模型请求、压缩与错误重试策略
  tools.rs            # 工具流水线、调度、审批、协议闭合
  interrupt.rs        # steer/cancel 的结构化收敛
  turn.rs             # 生命周期、Session 提交、唯一终态
  command.rs          # 现有命令到 followup/steer/inject/control 的映射
```

实际文件可复用现有模块，不为匹配目录图做无收益搬迁。判断标准是每项状态只有一个权威归属。

## 11. 测试重组

### 11.1 行为层

保留并强化：直接回答、工具调用、明确需要工具时不虚假完成、取消、审批、持久化、用量、生命周期、唯一终态、插件反馈和 Provider 协议。

### 11.2 调度层

新增或改写：

1. 多个 followup FIFO 且只有一个 driver；
2. steer 取消当前活动并在下一 step 生效；
3. inject 不唤醒，下一自然 step 才生效；
4. turn 开始领取一条 next_turn，step 开始领取 next_step；
5. stopping 窗口到达的 next_step 不丢失；
6. 取消收敛窗口设置 `wake_requested` 后必定重放；
7. 并发唤醒不会重复 start；
8. turn B 在 turn A 提交后读取最新 Session；
9. driver 内部失败后，未消费 Inbox 仍有明确所有者或明确失败；
10. shutdown 不静默清除已确认输入；
11. 普通问答无工具义务时直接完成；
12. 明确命令、附件、代码修改和验证义务下的纯文本响应被门控拒绝；
13. Provider 工具约束生效，协议修复成功后正常执行；
14. 协议修复耗尽后明确失败且不发布成功终态。

### 11.3 策略层

压缩、请求重试、预算取消、工具义务判定、协议修复和审批服务分别做独立测试，不通过完整 Loop 阶段名断言内部实现。

### 11.4 删除旧策略测试

删除 Summary 触发、NeedMoreWork、ForceFinal、outer iteration 和 continuation limit 的控制流测试。若其中包含仍有效的取消、用量或消息断言，先抽离到对应行为测试。

## 12. 分阶段实施

### 阶段 A：需求与安全网

1. 更新需求、设计和 TODO，冻结新的删除清单。
2. 给现有测试分类：永久保留、按新架构改写、删除/迁移。
3. 先补 Inbox、唯一 driver、wake latch 和关闭语义的失败用例。

### 阶段 B：Inbox 与 Driver

1. 建立 Agent 自有 Inbox。
2. 建立 `Idle | Running` 原子启动和 `wake_requested`。
3. 用同一 driver 连续处理 turn。
4. 删除临时下一 turn 全局队列与每消息 spawn。
5. 确保下一 turn 从最新 Session 构建。

### 阶段 C：最小 Loop

1. 把无工具响应改为候选完成，建立确定性的 `TaskContract` / `CompletionGate`。
2. 对明确工具义务优先使用 Provider `tool_choice` 约束，漏发时执行有界协议修复，耗尽后明确失败。
3. 删除 Summary/ForceFinal 请求和阶段。
4. 删除 continuation/outer iteration 预算。
5. 收敛 `execute.rs` 为 turn/step driver。

### 阶段 D：策略外置

1. 把压缩迁到请求前/请求错误策略。
2. 把审批等待迁入工具流水线。
3. 把 Provider 重试和可选安全预算迁出 Loop。
4. 删除迁移期双轨和废弃模块。

### 阶段 E：验证与交付

1. 运行完整 Rust 格式、静态检查、核心与相关插件测试。
2. 实测连续 followup、运行中 steer、工具 inject、取消收敛和 Core 关闭。
3. 测量生产代码和权威入口数量。
4. 交付前审查无静默丢消息、重复 driver、过期 Session、虚假工具完成和重复生命周期路径。

## 13. 量化验收

| 项目 | 当前问题 | 目标 |
| --- | --- | --- |
| Agent 顶层状态 | 多个执行阶段承载全部子系统 | 对外仅 `Idle | Running` |
| 完成控制 | Summary + ForceFinal + continuation | 无工具为候选完成；确定性义务门控通过才提交 |
| 下一 turn 调度 | 临时队列 + 每消息 spawn | Agent Inbox + 单 driver |
| 取消竞态 | 补偿后台任务 | `wake_requested` 锁存 |
| Session 上下文 | 可能提前捕获旧快照 | turn 启动时读取最新 Session |
| 审批 | Loop 顶层阶段 | 工具流水线服务 |
| 压缩/重试 | Loop 顶层阶段与续接 | 请求策略 |
| `execute.rs` 生产代码 | 当前约 1657 行 | 目标不超过 1200 行，且只保留 driver 编排 |
| 旧策略字段 | completion/continuation/outer limit | 全部删除 |
| 工具漏调用 | 模型纯文本可能被当成完成 | tool_choice + TaskContract + 有界协议修复或明确失败 |
| 测试关注点 | 大量旧内部策略 | Inbox、边界、工具义务、公开行为和可靠性 |

若某项不能达到，必须说明不可替代的外部契约和剩余代码归属；不得继续以补偿分支规避架构决策。

## 14. 需求追踪

| 需求 | 设计落点 | 计划任务 | 验证 |
| --- | --- | --- | --- |
| ALR-001~009 | 第 3、5、6 节 | 13~16 | 单 driver、候选完成、工具义务与协议修复测试 |
| ALR-101~106 | 第 4 节 | 13~14 | Inbox、边界、wake latch 测试 |
| ALR-201~206 | 第 4、7 节 | 14、18 | 最新 Session、顺序、关闭可靠性测试 |
| ALR-301~307 | 第 6、8、9 节 | 15~17 | 工具义务、审批、压缩、重试、取消测试 |
| ALR-401~405 | 第 10~13 节 | 13~18 | 协议回归、代码量检查、全链路验证 |

## 15. 最终决策

本次简化不再以“尽可能保持旧 Loop 内部策略”为约束。真正需要保持的是用户可见结果、Session 数据、插件生命周期、工具协议、用量和可靠交付。

DeepSeek Harness 提供的关键参考不是某段代码，而是边界原则：**Loop 只调用模型、运行工具并重复；其余能力由 Loop 外部负责。** 天工按现有 Rust、Session 和插件约束做等价实现，并以删除旧控制概念和减少权威入口作为验收依据。
