# Agent 执行核心重构 — 进度记录

> 需求：[requirements.md](./requirements.md) ｜设计：[design.md](./design.md) ｜任务：[tasks/](./tasks/)

## 当前状态

- 当前阶段：**全部 12 个任务（00~11）完成，review 整改完成**。等待真实场景验证（GUI/CLI/Server 代表入口）与 PR 审查交付。
- 当前建议任务：真实场景验证 + 按项目流程创建 PR。
- 当前阻塞：无。
- 生产代码状态：任务 09 迟到结果与稳态——逐通道盘点后决策**不引入 intent_generation**
  （全部通道由结构化所有权 abort+join/shutdown+drop 或 ingress 门控覆盖，决策表写入 design.md 7.1）；
  补迁移日志（中断 from_phase、LLM 完成 to_phase+budget、封口提交）与 ExecutionBudget 摘要；
  压力测试：连续双引导（都保存、按序重启、锚点最新）、命令风暴（混合命令按序、取消终态、
  副作用生效、用量累计）。105 测试 ×2 + agent-team 10 + workspace clippy/check 通过。
- review 整改（2026-08-14，8d0ac58f）：下一轮交接可靠性三条丢消息路径全部修复——
  `push_next_turn` 返回结果、`requeue_next_turn_front` 保存失败按序放回、消费循环保存成功才移除；
  封口状态纳入可接收判断（`CommandIngress::is_accepting`），浏览器 watcher 注入被拒保留快照重试、
  agent-team 注入失败不虚报送达、用量累计被拒记录警告。108 测试 + agent-team 10 + browser 32 通过。
- 重构基线：`feature/agent-execution-core` @ `7c425bac`（`main` v0.14.3 干净基线，不含引导消息改动）。
- 引导消息（ALR-101）在重构任务 04/07 中由 `ExecutionPhase` 实现，不合并 `perf/inject-user-message`。
- 绿色基线：`cargo test -p tiangong-core --lib` → 89 passed；`execute.rs` 3616 行。

## 任务总览

| 编号 | 任务 | 状态 | 分支 | 提交 | 关键产物 |
| --- | --- | --- | --- | --- | --- |
| 00 | [基线冻结](./tasks/00-基线冻结.md) | 已完成 | feature/agent-execution-core | 7c425bac（基线） | main v0.14.3 干净基线、引导消息重构中实现、89 测试绿色 |
| 01 | [关键路径与不变量测试](./tasks/01-关键路径测试.md) | 已完成 | feature/agent-execution-core | 0470d743 / aeedcba6 | ALR-107(单消息)/108/109/302/111 安全网；引导/PendingFinish 留 04/07 |
| 02 | [驱动原型](./tasks/02-驱动原型.md) | 已完成 | feature/agent-execution-core | 187bfa0d / aeedcba6 | take/install + abort 等待结束 + InstallGuard 守卫，结论写回 design.md |
| 03 | [执行预算与阶段数据](./tasks/03-执行预算与阶段数据.md) | 已完成 | feature/agent-execution-core | cb180a48 | ExecutionBudget/Limits、阶段类型移入 phase.rs、continuation_count 改名 |
| 04 | [模型与完成度阶段](./tasks/04-模型与完成度阶段.md) | 已完成 | feature/agent-execution-core | b1667d57(04a) / 2c3e0fbf(04b) | 引导消息链路 + ExecutionPhase 单一状态机主循环 |
| 05 | [工具与审批阶段](./tasks/05-工具与审批阶段.md) | 已完成 | feature/agent-execution-core | a603a22b | 不变量断言 + 迁移日志 + ALR-103/并行批次测试 |
| 06 | [压缩阶段](./tasks/06-压缩阶段.md) | 已完成 | feature/agent-execution-core | de3badc1 | CompressionPhase 命名对齐 + 引导中断压缩测试 |
| 07 | [统一命令与暂定完成](./tasks/07-统一命令与暂定完成.md) | 已完成 | feature/agent-execution-core | 9c1acc2c | CommandEffect 统一处理器 + PendingFinish 语义 + 顺序测试 |
| 08 | [终态封口](./tasks/08-终态封口.md) | 已完成 | feature/agent-execution-core | 970112c3 / 8d0ac58f(review 整改) | CommandIngress 门控 + 下一轮队列可靠交接 |
| 09 | [迟到结果与稳态](./tasks/09-迟到结果与稳态.md) | 已完成 | feature/agent-execution-core | 9ae68269 | 不引入代际决策（design 7.1）+ 日志补全 + 压力测试 |
| 10 | [完成度策略](./tasks/10-完成度策略.md) | 已完成 | feature/agent-execution-core | 45958adc | CompletionPolicy 解耦；默认策略保持（无数据不切换） |
| 11 | [模块拆分与交付](./tasks/11-模块拆分与交付.md) | 已完成 | feature/agent-execution-core | 60facb3f / 8d0ac58f(review 整改) | command/interrupt 拆分；全链路验证通过；真实场景验证待用户执行 |

状态取值：未开始 / 进行中 / 已完成 / 阻塞。

## 依赖关系

```text
00 基线冻结
 └→ 01 测试安全网
     └→ 02 驱动原型
         └→ 03 预算与阶段数据
             └→ 04 模型与完成度
                 └→ 05 工具与审批
                     └→ 06 压缩
                         └→ 07 命令与 PendingFinish
                             └→ 08 终态封口
                                 └→ 09 稳态验证
                                     ├→ 10 完成度策略评估
                                     └→ 11 模块拆分与交付
```

任务 10 只有在任务 09 稳定后才允许改变默认完成度策略。任务 11 如需吸收任务 10 的模块结果，应在任务 10 决策完成后进行；若任务 10 决定保持现状，也要记录结论再进入最终拆分。

## 分支与提交策略

1. 先完成并合并当前引导消息功能，更新本地主线。
2. 从最新 `main`/`develop` 创建 `feature/agent-execution-core`。
3. 每个任务至少一个独立提交；任务 04~08 可进一步按内部阶段拆成多个小提交。
4. 每个提交必须保持编译和对应测试绿色。
5. 不自动提交、推送或合并；PR 按项目流程创建和审查。
6. 出现回归时回滚到上一个绿色提交，不在红色基线上继续叠加阶段迁移。

## 每任务记录模板

任务开始或完成时记录：

```text
状态：
分支：
基线提交：
完成提交：
改动范围：
完成需求：
验证命令与结果：
遗留问题：
下一个任务：
```

## 验证层级

### 每个任务最低验证

```bash
cargo fmt -- --check
cargo clippy -p tiangong-core --all-targets --tests --benches -- -D warnings
cargo test -p tiangong-core
```

### 跨模块和最终验证

```bash
cargo check --workspace
cargo clippy --workspace --all-targets --tests --benches -- -D warnings
cargo test -p tiangong-plugin-agent-team
```

### 真实场景

- 普通直接回答；
- 多轮工具执行；
- 审批批准、拒绝和 FullTrust；
- 压缩与上下文超限；
- Summary NeedMoreWork 和 ForceFinal；
- 连续引导和连续命令；
- 引导期间子 Agent/浏览器/终端后台保持；
- Cancel/Shutdown；
- 终态封口前后消息交接；
- GUI、CLI、Server 的代表入口。

## 已有里程碑

- 2026-08-14：完成第一版状态机收敛方案。
- 2026-08-14：根据工程评估升级为整体 Agent 执行核心重构方案，扩展为 00~11 任务。
- 2026-08-14：任务 00 基线冻结——确认 `main` v0.14.3 干净基线，引导消息（ALR-101）在重构中实现，89 测试绿色基线。
- 2026-08-14：任务 02 驱动原型——`react/phase.rs` 验证 take/install 所有权模式 + AbortHandle 取消（后补真实取消与守卫验证，共 4 测试），结论写回 design.md。
- 2026-08-14：review 整改（8d0ac58f）——修复终态封口与下一轮交接三条丢消息路径，封口状态纳入插件反馈可接收判断；108 测试 + agent-team 10 + browser 32 + workspace clippy 全绿。

## 更新规则

- 每开始一个任务：标记进行中，记录真实基线和分支。
- 每完成一个任务：记录提交、需求编号和实际验证，不只写“通过”。
- 设计发生变化：先更新需求/设计/任务，再继续生产代码。
- 遇到实现障碍但可自行解决：保持进行中，不标记外部阻塞。
- 只有缺少用户决定、授权、凭据或外部资源时标记阻塞。

## ALR 测试落点

| ALR | 落点任务 | 状态 |
| --- | --- | --- |
| 101 引导消息同 turn 重启 | 04（ExecutionPhase + InjectUserMessage） | 已覆盖（inject_user_message_interrupts_tools_and_restarts、consecutive_injects_are_all_saved_and_restart_in_order、final_status_anchors_to_injected_latest_user_message） |
| 102 预算重置 | 04（reset_for_new_intent） | 已覆盖（引导重启重置 request_round/continuation_count，见 ALR-101 测试） |
| 103 插件后台保持 | 04/07 | 已覆盖（inject_does_not_cancel_plugins_but_explicit_cancel_does） |
| 104 Summary 中断降级 | 04 | 已覆盖（interrupted_llm_summary_output_persists_as_react_phase、inject_during_compression_cancels_and_restarts_without_applying_summary） |
| 105 暂定完成可撤销 | 07 | 已覆盖（handles_runtime_feedback_while_request_is_running、reenters_agent_loop_when_plugin_result_arrives_during_summary：PendingFinish 收到工具注入撤销暂定重新分析） |
| 106 控制命令完整处理 | 07 | 已覆盖（consecutive_inject_then_cancel_terminates_in_order、command_storm_is_processed_in_order_without_panicking、reasoning_effort_update_applies_to_next_model_request） |
| 107 最新消息锚点 | 01（单消息）+ 04（多消息）| 已覆盖（单消息锚点 + 磁盘重载；final_status_anchors_to_injected_latest_user_message、consecutive_injects_are_all_saved_and_restart_in_order） |
| 108 生命周期唯一 | 01（mock 插件计数）| 已覆盖（run_turn_invokes_lifecycle_hooks_exactly_once）|
| 109 唯一终态 | 01（Done 计数）| 已覆盖（run_turn_emits_single_done_and_anchors_status_to_latest_user_message）|
| 110 工具协议闭合 | 01 基线 + 05/06 | 已覆盖（取消时闭合测试、parallel_tool_batch_executes_both_and_closes_protocol）|
| 111 用量权威 | 01（执行循环累计）+ 07（暂定完成晚到）+ turn 级 | 已覆盖（accumulated_usage_is_aggregated_across_requests；PendingFinish 提交时读取最新累计用量，execute.rs:896 提交路径） |

基线 execute_turn 回归网覆盖核心路径：直接回答、工具循环、Summary（Done/NeedMoreWork/AskUser）、ForceFinal、压缩续接、审批（批准/拒绝/FullTrust）、取消、请求失败、用量累计。引导消息与 PendingFinish 相关测试在 04/07 加入后已全部落地。

## 验证记录

| 任务 | fmt | clippy | test | 备注 |
| --- | --- | --- | --- | --- |
| 00 基线 | ✅ | ✅ | 89 通过 | main v0.14.3 干净基线 |
| 01 测试安全网 | ✅ | ✅ | 97 通过 | ALR-107/108/109/302/111 安全网（含 run_turn 级 + mock 插件）|
| 02 驱动原型 | ✅ | ✅ | 97 通过 | take/install + abort 等待结束 + InstallGuard 守卫 |
| 03 预算与阶段数据 | ✅ | ✅ | 93 通过 | 控制流未变；98-5 原型测试（原型按 spec 删除） |
| 04a 引导消息链路 | ✅ | ✅ | 96 通过（×2 稳定）| ALR-101/102/104/107 多消息；连跑两次无 flaky |
| 04b ExecutionPhase 主循环 | ✅ | ✅ | 96 通过（×2 稳定）| next_step/can_advance/并列 Option 清除；workspace check 通过 |
| 05 工具与审批阶段 | ✅ | ✅ | 98 通过（×2）+ agent-team 10 | 不变量断言、迁移日志、ALR-103、并行批次 |
| 06 压缩阶段 | ✅ | ✅ | 99 通过（×2）| CompressionPhase 对齐设计；引导中断压缩、迟到结果不应用 |
| 07 统一命令与暂定完成 | ✅ | ✅ | 100 通过（×2）| CommandEffect、InjectTool 撤销暂定、用量刷新、顺序测试 |
| 08 终态封口 | ✅ | ✅ | 103 通过（×2）+ agent-team 10 | ingress 门控/封口排空/下一轮队列测试 |
| 09 迟到结果与稳态 | ✅ | ✅ | 105 通过（×2）+ agent-team 10 | 代际不引入决策、日志、连续引导/命令风暴 |
| 10 完成度策略 | ✅ | ✅ | 107 通过（×2）| 策略解耦 + 判定表测试；默认行为不变 |
| 11 模块拆分 | ✅ | ✅ | 107 通过（×2）+ agent-team 10 + workspace | 纯机械移动；行为零变更 |
| review 整改（08/11） | ✅ | ✅ | 108 通过 + agent-team 10 + browser 32 + workspace clippy | 下一轮交接可靠性（push/requeue/消费循环）+ 插件反馈封口保护 |
