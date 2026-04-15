# 架构差距分析与补全计划

> 基准文档：`docs/desktop-agent-technical-architecture.md`
> 分析日期：2026-04-03
> 目标：对照架构文档 16 个层/能力，梳理当前实现差距，制定补全路线

---

## 一、差距总览

| # | 架构文档层 | 当前状态 | 差距等级 |
|---|-----------|---------|---------|
| 1 | 查询编排层（3.3） | 逻辑散落在 TurnRunner/RuntimeEngine | **高** |
| 2 | 上下文装配层（3.4） | 结构存在，缺长期记忆/检索注入 | 中 |
| 3 | 规划与路由层（3.5） | QueryMode 仅二分法 | 中 |
| 4 | 多代理协调层（3.7） | 框架存在，Worker 隔离不足 | 中 |
| 5 | 任务模型统一（5.3/10） | RunStatus 与 TaskStatus 两套模型 | **高** |
| 6 | 后台任务回流（10） | 后台命令执行有，回流/恢复缺 | **高** |
| 7 | 权限细粒度控制（9.2） | 工具级有，路径/网络级缺 | 中 |
| 8 | 远程角色模型（11） | Connector 框架有，角色区分无 | 低 |
| 9 | 恢复与持久化（13） | 会话持久化有，任务现场恢复缺 | **高** |
| 10 | 观测与成本治理（14） | 模块存在，三层成本治理未闭环 | 中 |

---

## 二、详细差距说明与补全方案

### GAP-1：查询编排层独立抽象

**当前问题**：
- 查询编排逻辑（是否打断运行任务、路由到直接回答/工具执行/多代理/后台运行）散落在 `TurnRunner` 和 `RuntimeEngine` 中
- `QueryMode` 只有 `DirectAnswer` / `ToolExecution` 两种，缺少多步执行/子任务拆分/后台运行等路由

**补全方案**：
1. 新建 `crates/tiangong-core/src/orchestrator/` 模块
2. 定义 `QueryOrchestrator` 作为控制中心，负责：
   - 接受当前事件，判断是否打断正在运行的任务
   - 裁剪或恢复会话状态
   - 路由到不同执行模式（扩展 `QueryMode`）
3. 扩展 `QueryMode` 枚举：
   - `DirectAnswer` — 直接回答
   - `SingleToolExecution` — 单工具执行
   - `MultiStepExecution` — 多步工具链执行
   - `TaskSplit` — 子任务拆分（多代理）
   - `BackgroundExecution` — 长任务后台运行
4. `TurnRunner` 改为从 `QueryOrchestrator` 获取路由决策

**涉及文件**：
- 新建：`src/orchestrator/mod.rs`, `src/orchestrator/query_orchestrator.rs`, `src/orchestrator/types.rs`
- 修改：`src/turn_runner.rs`, `src/context/assembler.rs`

---

### GAP-2：上下文装配层增强

**当前问题**：
- `context/` 有 assembler/budget/compressor/organizer，结构对齐
- 缺少"用户偏好/长期记忆"注入源
- 缺少"检索命中内容"注入源

**补全方案**：
1. 在 `context/` 中新增 `memory.rs` — 用户偏好与长期记忆管理
   - 从 `~/.tiangong/memory/` 加载用户偏好
   - 支持按会话/全局两级存储
2. 在 `context/assembler.rs` 中增加记忆注入步骤
3. 预留检索接口（`context/retriever.rs`），为未来 RAG 能力做准备

**涉及文件**：
- 新建：`src/context/memory.rs`, `src/context/retriever.rs`
- 修改：`src/context/assembler.rs`, `src/context/mod.rs`

---

### GAP-3：规划与路由层增强

**当前问题**：
- `QueryMode` 只有二分法，无法表达复杂路由意图

**补全方案**：
- 此项与 GAP-1 合并实现，通过扩展 `QueryMode` 和 `QueryOrchestrator` 的路由逻辑完成

---

### GAP-4：多代理 Worker 隔离增强

**当前问题**：
- `coordinator/worker.rs` 存在但 Worker 缺少独立上下文边界、独立工具集、预算上限

**补全方案**：
1. 在 `coordinator/types.rs` 的 `WorkerContext` 中增加：
   - `allowed_tools: Vec<String>` — 可用工具白名单
   - `context_boundary: Vec<Message>` — 独立上下文（不共享主会话）
   - `budget: WorkerBudget` — 包含 `max_tokens`/`max_rounds`/`max_tool_calls`
2. Worker 执行时通过 `PermissionGate` 限制工具集
3. Worker 结果汇总时做 budget 超限检查

**涉及文件**：
- 修改：`src/coordinator/types.rs`, `src/coordinator/worker.rs`, `src/coordinator/task_coordinator.rs`

---

### GAP-5：统一任务模型

**当前问题**：
- `runtime.rs` 的 `RunStatus`（Idle/Planning/Executing/WaitingApproval/Completed/Failed）
- `tool/background_task.rs` 的 `TaskStatus`（Running/Completed/Failed/Cancelled）
- 两套模型未统一，缺少 `queued`/`blocked`/`backgrounded` 状态

**补全方案**：
1. 新建 `crates/tiangong-core/src/task/` 模块，定义统一任务模型
2. `UnifiedTaskStatus` 枚举覆盖文档 10.2 完整状态图：
   - `Queued` → `Running` → `Completed`/`Failed`/`Cancelled`
   - `Running` → `WaitingApproval` → `Running`
   - `Running` → `Backgrounded` → `Running`
   - `Running` → `Blocked` → `Running`
3. `UnifiedTask` 结构包含：输入摘要、负责代理、当前进度、结果位置、关联会话、关联工作目录
4. 迁移 `RunStatus` 和 `TaskStatus` 统一使用 `UnifiedTaskStatus`
5. `BackgroundTask` 纳入统一任务管理

**涉及文件**：
- 新建：`src/task/mod.rs`, `src/task/model.rs`, `src/task/registry.rs`, `src/task/state_machine.rs`
- 修改：`src/runtime.rs`, `src/tool/background_task.rs`

---

### GAP-6：后台任务回流与通知

**当前问题**：
- `tool/background_task.rs` 只支持后台命令执行（Child 进程）
- 后台任务完成后无法自动回流到会话
- 缺少后台任务恢复能力

**补全方案**：
1. 在统一任务模型（GAP-5）基础上，实现任务完成通知
2. 后台任务完成时生成 `RuntimeEvent`（`TaskCompleted`/`TaskFailed`），通过 EventBus 发布
3. `TurnRunner` 订阅后台任务事件，将结果注入当前会话上下文
4. 后台任务状态持久化到 `~/.tiangong/tasks/`，支持崩溃后恢复

**涉及文件**：
- 修改：`src/tool/background_task.rs`, `src/event.rs`, `src/turn_runner.rs`
- 新建：`src/task/notification.rs`, `src/task/persistence.rs`

---

### GAP-7：权限细粒度控制

**当前问题**：
- 当前只有工具级权限分级（Safe/Standard/Elevated/Critical）
- 缺少路径级规则、网络目标限制、数据出境限制等

**补全方案**：
1. 在 `permission.rs` 中扩展 `PermissionPolicy`：
   - `path_rules: Vec<PathRule>` — 路径级规则（允许/拒绝特定目录）
   - `network_rules: Vec<NetworkRule>` — 网络目标限制（域名/IP 白名单）
2. `PermissionGate::check()` 扩展为接受工具参数，检查路径和网络规则
3. 审计记录增加参数摘要字段

**涉及文件**：
- 修改：`src/permission.rs`, `src/observe/audit.rs`

---

### GAP-8：远程接入角色模型

**当前问题**：
- 早期方案基于 `tiangong-gateway` 设计远程角色与入口
- 文档仍停留在“控制者/审批者/观察者”三角色模型
- 当前实现已收敛为 `controller / observer`，但历史文档未同步

**补全方案**：
1. 共享远程类型迁入 `tiangong-types/src/remote.rs`
2. 远程入口路由收敛到 `tiangong-server/src/remote/`
3. Server 模式强制 `full_trust`，远程角色仅保留：
   - `Controller` — 完整控制权
   - `Observer` — 只读查看
4. 风险控制通过部署边界、鉴权和会话可见范围完成，而不是远程审批

**涉及文件**：
- `crates/tiangong-types/src/remote.rs`
- `crates/tiangong-server/src/auth.rs`
- `crates/tiangong-server/src/remote/router.rs`

---

### GAP-9：恢复与持久化增强

**当前问题**：
- 会话持久化已有（repository/），但缺少：
  - 崩溃恢复（恢复执行中的任务现场）
  - 权限待处理项持久化
  - 工具执行记录持久化

**补全方案**：
1. 基于统一任务模型（GAP-5），任务状态实时持久化到 `~/.tiangong/tasks/{task_id}.json`
2. 启动时扫描 `tasks/` 目录，恢复未完成任务的现场：
   - `Running`/`Backgrounded` 状态的任务标记为 `interrupted`，通知用户
   - `WaitingApproval` 状态的任务恢复审批界面
3. 工具执行记录持久化到 `~/.tiangong/tool-logs/`
4. 权限待处理项持久化到任务状态中

**涉及文件**：
- 新建：`src/task/persistence.rs`（与 GAP-6 共用），`src/task/recovery.rs`
- 修改：`src/app_state/repository/`

---

### GAP-10：观测与成本治理闭环

**当前问题**：
- `observe/` 有 metrics/cost/audit 三个模块
- 缺少请求级/任务级/会话级三层成本聚合
- 缺少统一采集入口

**补全方案**：
1. `CostSummary` 扩展为三层结构：
   - `RequestCost` — 单次 LLM 调用成本
   - `TaskCost` — 单任务累计成本（聚合所有 RequestCost）
   - `SessionCost` — 整轮工作累计成本
2. 在 `observe/` 中新增 `collector.rs` 统一采集入口
3. `MetricsCollector` 集成到 `TurnRunner`，自动采集每个阶段的延迟和成本

**涉及文件**：
- 修改：`src/observe/cost.rs`, `src/observe/metrics.rs`
- 新建：`src/observe/collector.rs`

---

## 三、实施优先级与阶段划分

### Phase A：基础设施（高优先级）
> 统一任务模型和查询编排是后续所有能力的基础

1. **GAP-5**：统一任务模型 — 所有其他 GAP 依赖此基础
2. **GAP-1/3**：查询编排层独立抽象 + 路由增强

### Phase B：执行闭环（高优先级）
> 后台任务回流和恢复是产品可用性的关键

3. **GAP-6**：后台任务回流与通知
4. **GAP-9**：恢复与持久化增强

### Phase C：能力增强（中优先级）
> 上下文、多代理、权限的精细化提升

5. **GAP-2**：上下文装配层增强
6. **GAP-4**：多代理 Worker 隔离
7. **GAP-7**：权限细粒度控制
8. **GAP-10**：观测与成本治理

### Phase D：远程能力（低优先级）
> 依赖前面所有能力就绪

9. **GAP-8**：远程接入角色模型

---

## 四、预期成果

补全完成后，架构将覆盖文档定义的所有 16 个层/能力：
- 输入闭环：用户/远程/系统事件统一进入会话入口 ✅
- 推理闭环：查询编排/上下文装配/模型访问循环运行 ✅（补全查询编排）
- 执行闭环：工具/权限/结果/通知回流形成操作链 ✅（补全回流）
- 任务闭环：前台/后台/恢复/持久化统一建模 ✅（补全任务模型）
- 扩展闭环：技能/插件/协议按需装配 ✅
