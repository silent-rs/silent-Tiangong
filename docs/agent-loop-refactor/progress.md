# Agent 执行核心重构 — 进度记录

> 需求：[requirements.md](./requirements.md) ｜设计：[design.md](./design.md) ｜任务：[tasks/](./tasks/)

## 当前状态

- 当前阶段：任务 04 进行中——04a 引导消息命令链路完成，04b ExecutionPhase 主循环改造待开始。
- 当前建议任务：**04b - ExecutionPhase 主循环改造（ExecutionEvent 归一化先行，见 design.md 3 节）**。
- 当前阻塞：无。
- 生产代码状态：04a 落地 InjectUserMessage 命令 + deliver 注入路径 + interrupt_active_work/save_user_message_and_restart + run_turn 锚点改为提交时最新用户消息。core lib 96 测试通过。
- 重构基线：`feature/agent-execution-core` @ `7c425bac`（`main` v0.14.3 干净基线，不含引导消息改动）。
- 引导消息（ALR-101）在重构任务 04/07 中由 `ExecutionPhase` 实现，不合并 `perf/inject-user-message`。
- 绿色基线：`cargo test -p tiangong-core --lib` → 89 passed；`execute.rs` 3616 行。

## 任务总览

| 编号 | 任务 | 状态 | 分支 | 提交 | 关键产物 |
| --- | --- | --- | --- | --- | --- |
| 00 | [基线冻结](./tasks/00-基线冻结.md) | 已完成 | feature/agent-execution-core | 7c425bac（基线） | main v0.14.3 干净基线、引导消息重构中实现、89 测试绿色 |
| 01 | [关键路径与不变量测试](./tasks/01-关键路径测试.md) | 已完成 | feature/agent-execution-core | 0470d743 / aeedcba6 | ALR-107(单消息)/108/109/302/111 安全网；引导/PendingFinish 留 04/07 |
| 02 | [驱动原型](./tasks/02-驱动原型.md) | 已完成 | feature/agent-execution-core | 187bfa0d / aeedcba6 | take/install + abort 等待结束 + InstallGuard 守卫，结论写回 design.md |
| 03 | [执行预算与阶段数据](./tasks/03-执行预算与阶段数据.md) | 已完成 | feature/agent-execution-core | （本次） | ExecutionBudget/Limits、阶段类型移入 phase.rs、continuation_count 改名 |
| 04 | [模型与完成度阶段](./tasks/04-模型与完成度阶段.md) | 进行中（04a 完成） | feature/agent-execution-core | （本次 04a） | InjectUserMessage 链路 + 中断保存重启 + ALR-101/102/104/107 测试 |
| 05 | [工具与审批阶段](./tasks/05-工具与审批阶段.md) | 未开始 | - | - | 工具批次与审批状态机 |
| 06 | [压缩阶段](./tasks/06-压缩阶段.md) | 未开始 | - | - | 压缩续接状态机 |
| 07 | [统一命令与暂定完成](./tasks/07-统一命令与暂定完成.md) | 未开始 | - | - | 单一命令语义、删除旧状态 |
| 08 | [终态封口](./tasks/08-终态封口.md) | 未开始 | - | - | ingress、可靠下一 turn 交接 |
| 09 | [迟到结果与稳态](./tasks/09-迟到结果与稳态.md) | 未开始 | - | - | 并发决策、日志、压力验证 |
| 10 | [完成度策略](./tasks/10-完成度策略.md) | 未开始 | - | - | CompletionPolicy、Summary 评估 |
| 11 | [模块拆分与交付](./tasks/11-模块拆分与交付.md) | 未开始 | - | - | 模块化核心与最终交付 |

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

## 更新规则

- 每开始一个任务：标记进行中，记录真实基线和分支。
- 每完成一个任务：记录提交、需求编号和实际验证，不只写“通过”。
- 设计发生变化：先更新需求/设计/任务，再继续生产代码。
- 遇到实现障碍但可自行解决：保持进行中，不标记外部阻塞。
- 只有缺少用户决定、授权、凭据或外部资源时标记阻塞。

## ALR 测试落点

| ALR | 落点任务 | 状态 |
| --- | --- | --- |
| 101 引导消息同 turn 重启 | 04（ExecutionPhase + InjectUserMessage） | 待实现 |
| 102 预算重置 | 04（reset_for_new_intent） | 待实现 |
| 103 插件后台保持 | 04/07 | 待实现 |
| 104 Summary 中断降级 | 04 | 待实现 |
| 105 暂定完成可撤销 | 07 | 待实现 |
| 106 控制命令完整处理 | 07 | 待实现 |
| 107 最新消息锚点 | 01（单消息）+ 04（多消息）| 部分覆盖：单消息锚点 + 磁盘重载已测；多消息（引导注入后写最新、不覆盖旧消息、重载一致）留任务 04 验收 |
| 108 生命周期唯一 | 01（mock 插件计数）| 已覆盖（run_turn_invokes_lifecycle_hooks_exactly_once）|
| 109 唯一终态 | 01（Done 计数）| 已覆盖（run_turn_emits_single_done_and_anchors_status_to_latest_user_message）|
| 110 工具协议闭合 | 01 基线 + 05/06 | 基线覆盖（取消时闭合测试）|
| 111 用量权威 | 01（执行循环累计）+ 07（暂定完成晚到）+ turn 级 | 执行循环累计已覆盖；Session/Done 事件/暂定完成晚到用量待补 |

基线 execute_turn 回归网覆盖核心路径：直接回答、工具循环、Summary（Done/NeedMoreWork/AskUser）、ForceFinal、压缩续接、审批（批准/拒绝/FullTrust）、取消、请求失败、用量累计。引导消息与 PendingFinish 相关测试因基线无对应命令/阶段，在 04/07 加入后补。

## 验证记录

| 任务 | fmt | clippy | test | 备注 |
| --- | --- | --- | --- | --- |
| 00 基线 | ✅ | ✅ | 89 通过 | main v0.14.3 干净基线 |
| 01 测试安全网 | ✅ | ✅ | 97 通过 | ALR-107/108/109/302/111 安全网（含 run_turn 级 + mock 插件）|
| 02 驱动原型 | ✅ | ✅ | 97 通过 | take/install + abort 等待结束 + InstallGuard 守卫 |
| 03 预算与阶段数据 | ✅ | ✅ | 93 通过 | 控制流未变；98-5 原型测试（原型按 spec 删除） |
| 04a 引导消息链路 | ✅ | ✅ | 96 通过（×2 稳定）| ALR-101/102/104/107 多消息；连跑两次无 flaky |
