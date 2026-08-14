# Agent 执行核心重构 — 进度记录

> 需求：[requirements.md](./requirements.md) ｜设计：[design.md](./design.md) ｜任务：[tasks/](./tasks/)

## 当前状态

- 当前阶段：**00~10 历史迁移已完成；11 暂停交付；12 简化需求与设计已完成；13 测试契约重组已完成；14~18 待实施**。
- 当前建议任务：任务 14，建立 Agent 自有 Inbox（next_turn/next_step）、唯一 driver、`wake_requested` 与最新 Session 启动，删除临时 `NEXT_TURN_QUEUE` 与每消息后台任务，并启用 `core/contract_tests.rs` 的 4 项失败用例。
- 当前阻塞：无。
- 当前分支：`feature/agent-execution-core`，基线为 `main` v0.14.3（`7c425bac`）。
- 当前实现不是交付候选：Review 已确认旧 Summary/ForceFinal/continuation 控制仍在，下一 turn 的临时队列与每消息后台自动启动也存在结构性风险。
- 当前文档决策：Loop 收敛为模型—工具最小循环；无工具响应只形成候选完成；通过确定性 `TaskContract`、Provider 工具约束和有界协议修复防止模型漏发必需 tool call；Agent 自有 Inbox 与单 driver；审批、压缩、重试和预算外置。
- 任务 13 已建立新契约测试安全网：`react/contract_tests.rs`（工具义务 5 项，1 绿 4 ignore）与 `core/contract_tests.rs`（调度可靠性 4 项 ignore）；ignored 用例已核对在当前实现上全部失败，任务 14/15 实现后必须转绿。

## 已确认的 Review 结论

1. 相对 `origin/main`，Rust 新增约 3647 行、删除约 1068 行，净增约 2579 行；增长主要来自兼容旧行为、下一 turn 交接补偿和测试。
2. `execute.rs` 从 3616 行增至 4515 行；生产部分约从 1744 行降至 1657 行，测试增长较多，但主干职责仍未充分简化。
3. `execute.rs` 旧测试保留率约 96%：重构前 27 项，当前 40 项，保留 26 项，仅明显替换 1 项。大部分旧 Summary/ForceFinal 控制测试仍在，说明旧 Loop 控制模型没有真正淘汰。
4. 当前下一 turn 实现经历两轮补偿后仍有风险：
   - 可能提前构建并持有旧 Session 快照；
   - 后台失败后消息重新入队但没有持续调度者；
   - 抢先启动竞争可能重复确认或重复启动；
   - Core 关闭会静默清除已经返回成功的排队消息；
   - 每条消息 spawn 一次性后台任务，调度权威入口不唯一。
5. 原设计红线规定交接复杂度失控时回退并重新评审 ALR-202；当前情况已经满足该条件。
6. DeepSeek Harness 对照结果：Loop 只负责调用模型、执行工具并重复；Inbox、压缩、审批、重试和失控控制分别归属其他边界，没有 Summary、ForceFinal 或内置轮次预算。
7. 天工真实场景补充约束：模型可能在任务明确需要命令、附件、文件或验证工具时返回纯文本，因此不能直接采用“无工具即完成”；必须把它降为候选完成，并检查可程序判定的工具义务。
8. 天工早期单循环历史核对：
   - `db18a318`（2026-05-09）是清晰的单 `'react_loop`，`engine.rs` 约 614 行；无工具响应直接保存并 `Done`；
   - `ca9e465b` 增加“有 reasoning 且短回复”启发式重试，覆盖面不足；
   - `06c0ac18` 改为 lite 模型完成判断，最多两次重入，仍缺少真实工具证据；
   - 双阶段引入前 `474389d0^` 的单循环已增长到约 2428 行，说明单循环本身不自动带来模块简洁；
   - `474389d0` 引入 ReAct + Summary 后 `engine.rs` 约 2853 行，单次新增 1399 行、删除 974 行，并形成后来延续的 outer/ForceFinal 控制。
9. 决策：新方案恢复早期单循环的控制形态，但不恢复“无工具即成功”、文本启发式、lite 完成模型、所有子系统内联或 max rounds 后强制回复；改用 TaskContract、原生工具约束、有界协议修复和明确失败。

## 任务总览

| 编号 | 任务 | 状态 | 提交/基线 | 关键结果 |
| --- | --- | --- | --- | --- |
| 00 | 基线冻结 | 历史已完成 | 7c425bac | main v0.14.3 干净基线、89 项核心测试 |
| 01 | 关键路径与不变量测试 | 历史已完成 | 0470d743 / aeedcba6 | 生命周期、唯一终态、用量、锚点安全网 |
| 02 | 驱动原型 | 历史已完成 | 187bfa0d / aeedcba6 | take/install 与结构化取消原型 |
| 03 | 执行预算与阶段数据 | 历史已完成 | cb180a48 | 统一阶段数据，但保留 continuation 控制 |
| 04 | 模型与完成度阶段 | 历史已完成 | b1667d57 / 2c3e0fbf | 引导链路与 ExecutionPhase 主循环 |
| 05 | 工具与审批阶段 | 历史已完成 | a603a22b | 工具批次、并行与审批阶段 |
| 06 | 压缩阶段 | 历史已完成 | de3badc1 | CompressionPhase 与续接路径 |
| 07 | 统一命令与暂定完成 | 历史已完成 | 9c1acc2c | CommandEffect 与 PendingFinish |
| 08 | 终态封口与下一 turn | 历史已完成，待替换 | 970112c3 / 8d0ac58f / be0c7c96 | 当前临时队列与后台自动启动方案 |
| 09 | 迟到结果与稳态 | 历史已完成 | 9ae68269 | 结构化取消盘点与压力测试 |
| 10 | 完成度策略 | 历史已完成，待删除旧策略 | 45958adc | CompletionPolicy 解耦但默认行为仍保留 |
| 11 | 模块拆分与交付 | 暂停交付 | 60facb3f 等 | 代码与自动化检查通过，但真实验证和架构 Review 未通过 |
| 12 | 简化需求与设计 | 已完成 | 69c884c5 | Harness 对照、需求重定界、删除清单、量化验收 |
| 13 | 测试契约重组 | 已完成 | 本次提交 | 三分类清单 + 9 项契约测试（1 绿 + 8 ignore 失败用例，均核对失败形态） |
| 14 | Agent Inbox 与唯一 driver | 未开始 | — | next_turn/next_step、Idle/Running、wake_requested、最新 Session |
| 15 | 最小 Loop | 未开始 | — | 模型—工具循环、TaskContract/CompletionGate、删除旧完成控制 |
| 16 | 模型请求策略外置 | 未开始 | — | tool_choice、有界协议修复、压缩、重试和安全预算 |
| 17 | 工具流水线收敛 | 未开始 | — | 审批下沉、并行/屏障/顺序提交/协议闭合 |
| 18 | 清理、验收与交付 | 未开始 | — | 删除双轨、量化、真实场景、完整检查和 PR |

状态取值：未开始 / 进行中 / 文档完成，待提交 / 历史已完成 / 暂停交付 / 已完成 / 阻塞。

## 新依赖关系

```text
00~10 历史迁移
      │
      ├─ 11 暂停交付
      │
      ▼
12 简化需求与设计
      │
      ▼
13 测试契约重组
      │
      ▼
14 Inbox 与唯一 driver
      │
      ▼
15 最小 Loop
      │
      ├────────────┐
      ▼            ▼
16 请求策略外置   17 工具流水线收敛
      └──────┬─────┘
             ▼
18 清理、验收与交付
      │
      └─ 完成后同步勾选 11
```

任务 14 必须先替换当前临时下一 turn 调度，任务 15 才删除旧完成控制；任务 16 和 17 可在任务 15 稳定后按独立提交推进。任务 18 未通过前不得恢复任务 11 的 PR 交付。

## 测试迁移清单

### 永久保留

- 直接回答、工具后回答，以及明确工具义务下不会虚假完成；
- 工具协议闭合、并行、独占与取消；
- 审批批准、拒绝、FullTrust 和显式取消；
- 生命周期唯一、最终终态唯一；
- Session 持久化、最新消息锚点和累计用量；
- 插件反馈、标题、配置、流事件和请求失败；
- 子 Agent、浏览器和终端后台任务的既有语义。

### 按新架构改写

- 连续用户输入 → followup FIFO、同一 driver 连续 turn；
- 运行中引导 → steer 在下一 step 生效；
- 工具/插件注入 → inject 不唤醒、下一自然 step 生效；
- PendingFinish/封口竞态 → stopping Inbox 检查；
- 取消与新消息竞态 → `wake_requested` 重放；
- 自动下一轮测试 → 最新 Session 构建、至多一个 driver、失败后仍有权威所有者；
- Core 关闭 → 不接受新输入、不静默清除已确认输入；
- 工具漏调用 → 无义务时直接完成，有义务时纯文本被拒绝；
- 工具约束与修复 → `required`/指定工具生效，修复成功继续执行，耗尽后明确失败；
- 义务证据 → 工具成功并持久化才满足义务，模型文字声明和工具失败都不能冒充证据。

### 删除或迁移到独立策略测试

- `runs_tool_then_completes_via_summary`；
- `reenters_agent_loop_when_summary_needs_more_work`；
- `summary_tool_calls_continue_without_consuming_summary_iteration`；
- `invalid_summary_tool_calls_trigger_another_bounded_iteration`；
- `forces_final_response_after_summary_request_fails`；
- `returns_failed_when_summary_and_force_final_both_fail`；
- `force_final_does_not_commit_empty_reply_from_invalid_tool_call`；
- `returns_cancelled_when_summary_is_cancelled`；
- outer iteration / continuation limit 强制最终响应相关测试。

删除前先抽离其中仍有效的取消、用量、持久化和错误断言。

## 量化验收基线

| 项目 | 当前 | 任务 18 目标 |
| --- | --- | --- |
| `execute.rs` 总行数 | 约 4515 | 不以总行数单独验收，测试可迁移到独立模块 |
| `execute.rs` 生产代码 | 约 1657 | 目标不超过 1200，且只保留 driver 编排 |
| Agent 顶层状态 | 多个 ExecutionPhase 子系统阶段 | 对外 `Idle | Running` |
| 完成控制 | Summary / ForceFinal / continuation | 无工具为候选完成；工具义务门控通过才提交 |
| 下一 turn | 临时队列 + 每消息 spawn | Agent Inbox + 单 driver |
| 取消收敛 | 额外补偿调度 | `wake_requested` |
| Session | 存在未来 turn 旧快照风险 | turn 启动时读取最新 Session |
| 工具漏调用 | 纯文本可能被当成完成 | tool_choice + TaskContract + 有界修复或明确失败 |
| 权威入口 | 多处分散 | driver 启动、Session 写入、next_turn 领取各一个 |

## 验证要求

### 每个代码任务最低验证

```bash
cargo fmt -- --check
cargo clippy -p tiangong-core --all-targets --tests --benches -- -D warnings
cargo test -p tiangong-core
```

### 跨模块和最终验证

```bash
cargo check --workspace
cargo clippy --workspace --all-targets --tests --benches -- -D warnings
cargo test -p tiangong-core
cargo test -p tiangong-plugin-agent-team
cargo test -p tiangong-plugin-browser
```

### 真实场景

- 普通直接回答；
- 多轮工具执行；
- 明确要求执行命令但模型首次只返回说明文字；
- 指定附件未读取、代码改动未验证时阻止成功提交；
- Provider 工具修复成功以及连续漏调用后明确失败；
- 多条 followup FIFO；
- 模型等待、工具执行、审批和压缩期间 steer；
- inject 在自然 step 边界生效；
- 审批批准、拒绝和 FullTrust；
- 上下文压力压缩与溢出恢复；
- 取消收敛窗口同时到达新输入；
- driver 内部失败后 Inbox 仍可继续或明确失败；
- Core 关闭时已确认消息不静默丢失；
- GUI、CLI、Server 代表入口。

## 最近实际验证

当前生产代码最近一次 Review 前验证均通过：

- `cargo fmt -- --check`；
- `cargo clippy --workspace --all-targets --tests --benches -- -D warnings`；
- `cargo test -p tiangong-core`：109 项通过；
- `cargo test -p tiangong-plugin-agent-team`：10 项通过，1 项手动诊断测试 ignored；
- `cargo test -p tiangong-plugin-browser`：32 项通过；
- `git diff --check origin/main...HEAD`。

这些结果只证明当前代码可编译且旧行为测试通过，**不代表简化架构已经完成，也不解除任务 11 的暂停交付状态**。

任务 12 文档验证：

- `git diff --check`：通过；
- Markdown 本地链接检查：通过；
- 未运行 Rust 构建和测试，因为任务 12 未修改生产代码。

任务 13 验证（新增测试契约，未改生产行为）：

- `cargo fmt -- --check`：通过；
- `cargo clippy -p tiangong-core --all-targets --tests --benches -- -D warnings`：通过；
- `cargo test -p tiangong-core`：110 通过、8 ignored、0 失败（109 项既有 + 1 项新增绿色锚点）；
- `cargo test -p tiangong-core contract_tests -- --ignored`：8 项失败用例全部失败，失败断言逐条对应旧方案缺陷（`当前实现: Success`、`Err(WorkerStopped)`、请求不含前一 turn 标记消息、消息无独立 turn 状态、关闭后发出模型请求）。

## 分支与提交策略

1. 保持当前 `feature/agent-execution-core` 作为 Review 整改分支；不自动提交、推送或合并。
2. 任务 12 文档先独立审查；任务 13~18 每项至少一个独立提交。
3. 每个代码提交必须保持对应检查绿色。
4. 不为兼容迁移长期保留双轨；每个临时适配都必须在同一任务或明确的下一任务删除。
5. 出现回归时回到上一个绿色提交，不在红色基线上叠加修复。
6. 每完成一项记录真实提交、代码量、验证命令和遗留问题。

## 已有里程碑

- 2026-08-14：完成第一版状态机收敛方案及任务 00~10。
- 2026-08-14：完成两轮下一 turn 可靠性 Review 整改，自动化检查保持绿色。
- 2026-08-14：进一步 Review 确认旧 Loop 控制几乎完整保留，下一 turn 补偿调度复杂度继续增长，暂停任务 11 交付。
- 2026-08-14：完成 DeepSeek Harness 本地代码与文档对照分析。
- 2026-08-14：完成天工早期单循环历史实现对照，确认直接完成、短文本启发式和 lite 模型三代方案的边界。
- 2026-08-14：完成任务 12 简化需求、设计、TODO、PLAN 和进度同步，尚未修改生产代码。
- 2026-08-14：完成任务 13 测试契约重组：现有测试三分类完成，新增 9 项契约测试（`react/contract_tests.rs`、`core/contract_tests.rs`），8 项失败用例以 `#[ignore]` 挂起并核对失败形态。

## 更新规则

- 每开始一个任务：标记进行中，记录真实基线和分支。
- 每完成一个任务：记录提交、需求编号和实际验证，不只写“通过”。
- 设计发生变化：先更新需求、设计、PLAN、TODO 和本进度，再继续生产代码。
- 遇到可自行解决的实现问题：保持进行中，不标记外部阻塞。
- 只有缺少用户决定、授权、凭据或不可替代的外部资源时标记阻塞。
