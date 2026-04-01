# 天工架构重构计划：事件驱动状态机

## Context

对比新技术架构文档（`docs/desktop-agent-technical-architecture.md`），当前系统存在以下核心差距：
- 没有统一事件模型，各类输入走不同逻辑分支
- 核心执行链路是硬编码的 ReAct 循环（`runtime.rs` 1556行），不可中断、不可恢复
- 没有权限审批流程，命令/文件操作零权限控制
- 上下文装配散落在各 agent 的 prompt 中，缺少独立模块
- 没有后台任务、多代理、观测成本等能力

本次重构采用**分阶段增量推进**，每阶段一个独立 PR，功能不回退。核心目标是将 ReAct 循环替换为**事件驱动状态机**，并在此基础上逐步补齐权限、上下文装配、观测等架构层。

## 与 Connector 的兼容性

当前 Connector 的消息流：`Connector → MessageRouter → TiangongState.send_current_input() → RuntimeEngine → 轮询结果 → Connector.send_message()`

重构后的兼容保证：
- **Phase 1-2**：纯新增类型和权限层，不改 TiangongState API，Connector 零影响
- **Phase 3**：`TurnRunner` 替换 `RuntimeEngine` 内部实现，但对外接口不变（`send_current_input` / `poll_pending_turn` / `TurnEvent` 类型保持兼容）。MessageRouter 轮询逻辑无需修改
- **Phase 4**：上下文装配和任务状态扩展，Connector 不感知
- **Phase 5**：多代理执行透过 TurnEvent 回流结果，MessageRouter 仍通过轮询 `poll_pending_turn()` 获取最终 assistant 消息

**统一事件模型的额外收益**：
- Phase 1 的 `RuntimeEvent` 可与 Gateway 层的 `TiangongEvent` 整合，实现 Connector 消息→统一事件→执行→结果事件→Connector 回复的完整闭环
- Connector 收到的消息可自然转换为 `RuntimeEvent::UserMessage`
- 执行完成后的 `RuntimeEvent::TaskCompleted` 可触发 `TiangongEvent::MessageSent` 回流到 Connector

## 阶段总览

```
Phase 1 (事件模型 + 状态类型)
   │
   └──► Phase 2 (权限层 + 审批流程)
           │
           └──► Phase 3 (状态机替换 ReAct 循环) ★核心★
                   │
                   ├──► Phase 4 (上下文装配层 + 任务状态机)
                   │
                   └──► Phase 5 (多代理骨架 + 观测基础)
```

---

## Phase 1：统一事件模型 + 状态类型定义

**目标**：引入统一事件和状态机类型，纯新增，不改执行链路。

### 新增文件

**`crates/tiangong-core/src/event.rs`** — 统一运行时事件：
```rust
pub struct RuntimeEvent {
    pub event_id: String,           // scru128
    pub event_type: RuntimeEventType,
    pub session_id: String,
    pub task_id: Option<String>,
    pub source: EventSource,
    pub payload: serde_json::Value,
    pub created_at: String,
}

pub enum RuntimeEventType {
    UserMessage, ToolResult, PermissionRequest, PermissionResponse,
    TaskStarted, TaskCompleted, TaskFailed,
    LlmChunk, LlmOutput, Notification, SystemSignal,
}

pub enum EventSource { User, Runtime, Tool, Permission, System }
```

**`crates/tiangong-core/src/turn_state.rs`** — Turn 级状态机枚举：
```rust
pub enum TurnPhase {
    Init, ContextAssembly, LlmCalling, ToolDispatching,
    WaitingApproval { tool_name: String, request_id: String },
    ToolExecuting, ResultProcessing, Responding,
    Completed, Failed { error: String }, Cancelled,
}
```

### 修改文件
- `runtime.rs`：`RunStatus` 增加 `WaitingApproval` 变体
- `app_state/support.rs`：添加 `From<TurnEvent> for RuntimeEvent` 桥接转换
- `lib.rs`：导出新模块

### 不变
- 所有执行链路、前端、Tauri 命令层

---

## Phase 2：权限层 + 审批流程

**目标**：在工具执行前插入权限检查网关，默认"完全信任"模式。

### 新增文件

**`crates/tiangong-core/src/permission.rs`**：
```rust
pub enum TrustMode { FullTrust, Supervised }

pub enum PermissionLevel { Safe, Standard, Elevated, Critical }

pub struct PermissionPolicy {
    pub trust_mode: TrustMode,
    pub auto_approve: Vec<String>,   // 工具名白名单
    pub always_deny: Vec<String>,
}

pub struct PermissionGate { policy: PermissionPolicy }

impl PermissionGate {
    pub fn check(&self, tool_name: &str, args: &Value) -> PermissionDecision {
        // FullTrust → 直接 Approved
        // Supervised → 按 PermissionLevel 分级决策
    }
}

pub enum PermissionDecision { Approved, Denied { reason: String }, NeedsApproval { request_id: String } }
```

### 修改文件
- `runtime.rs`：`RuntimeEngine` 增加 `permission_gate` 字段，`execute_tool_call()` 前调用 `check()`
- `agent_config.rs`：增加 `trust_mode: TrustMode`（默认 `FullTrust`）
- `app_state/facade/runtime.rs`：构建 RuntimeEngine 时传入权限配置

### 审计
- 每次权限决策记录到审计日志（复用现有 `AuditEntry` 或新建 `PermissionAuditEntry`）

### 前端影响
- 无（FullTrust 默认，透明）
- 设置页可选择性暴露 TrustMode 切换

---

## Phase 3：事件驱动状态机替换 ReAct 循环 ★核心★

**目标**：将 `execute_turn_with_streaming()` 的硬编码 for 循环替换为显式状态机。

### 新增文件

**`crates/tiangong-core/src/turn_runner.rs`** — 状态机驱动的 Turn 执行器：

```
struct TurnRunner {
    phase: TurnPhase,
    engine: &RuntimeEngine,
    // session snapshot, context, messages, usage...
    event_tx: mpsc::Sender<TurnEvent>,
}

impl TurnRunner {
    fn run(mut self) {
        loop {
            match self.phase {
                Init → 构建工具定义和系统 prompt → ContextAssembly
                ContextAssembly → 装配上下文 → LlmCalling
                LlmCalling → 调用 LLM
                    → 无工具调用 → Responding
                    → 有工具调用 → ToolDispatching
                ToolDispatching → 逐个权限检查
                    → NeedsApproval → WaitingApproval (暂停，等外部事件)
                    → 全部通过 → ToolExecuting
                WaitingApproval → 等待审批响应 → ToolExecuting 或添加拒绝反馈
                ToolExecuting → 并发执行工具 → ResultProcessing
                ResultProcessing → 构建反馈、压缩上下文
                    → round < MAX → ContextAssembly（继续循环）
                    → round >= MAX → Responding
                Responding → 发送完成事件 → Completed
                Completed/Failed/Cancelled → break
            }
        }
    }
}
```

### 状态转换图
```
Init → ContextAssembly → LlmCalling
                              ↓
               ┌── 无工具 ──→ Responding → Completed
               └── 有工具 ──→ ToolDispatching
                                   ↓
                    ┌── 全通过 ──→ ToolExecuting → ResultProcessing
                    └── 需审批 ──→ WaitingApproval
                                       ↓
                          ┌── 批准 ──→ ToolExecuting
                          └── 拒绝 ──→ ContextAssembly（反馈拒绝原因）
ResultProcessing → round < MAX → ContextAssembly（下一轮）
                 → round >= MAX → Responding
```

### 重构 runtime.rs
- 移除 `execute_turn_with_streaming()` 方法（或保留为 `#[cfg(feature = "legacy")]`）
- `RuntimeEngine` 变为配置/工厂角色，提供 `create_turn_runner()` 方法
- 工具执行辅助方法（`execute_tool_call`、`handle_tts` 等）保持在 `RuntimeEngine` 上

### 修改文件
- `runtime.rs`：核心重构
- `app_state/services/turn/start.rs`：`thread::spawn` 改为 `runner.run()`
- `app_state/support.rs`：`TurnEvent` 增加 `PermissionResponse` 变体
- `app_state/facade/sessions/turn_control.rs`：轮询处理新状态
- `src-tauri/src/commands.rs`：新增 `approve_permission_request` 命令
- `src-tauri/src/types.rs`：`RunSnapshot` 处理 `WaitingApproval` 状态

### 兼容策略
- TurnRunner 发出与旧代码相同的 `TurnEvent` 序列
- 前端轮询层不感知内部状态机变化
- `WaitingApproval` 状态在前端可选处理（不处理则等同 Executing）

---

## Phase 4：上下文装配层 + 任务状态机完善

**目标**：上下文装配独立为可配置模块；任务状态对齐架构文档。

### 新增/重构文件

**`crates/tiangong-core/src/context/assembler.rs`** — 上下文装配协调器：
```
装配顺序：
1. 确定当前任务目标
2. 提取必要历史（ContextOrganizer）
3. 注入环境与权限事实
4. 注入用户偏好
5. 补充当前工作对象
6. 按需注入工具说明
7. Token 预算控制与压缩（ContextCompressor）
```

**`crates/tiangong-core/src/context/budget.rs`** — Token 预算控制器

### 扩展任务状态
`session.rs` 中 `SessionTaskStatus` 扩展：
```rust
pub enum SessionTaskStatus {
    Queued, Planning, Executing, Blocked,
    WaitingApproval, Backgrounded,
    Completed, Failed, Cancelled,
}
```

### 扩展 Session 模型
```rust
// 新增可选字段（#[serde(default)] 向后兼容）
pub active_task_id: Option<String>,
pub pending_approvals: Vec<PendingApproval>,
```

### 会话级工作目录独立管理

当前 `Session.cwd` 已有会话级工作目录字段，但存在以下不足：
- 新会话默认继承全局工作目录，缺少独立初始化策略
- 工具执行时的路径解析没有严格隔离到会话 cwd
- Connector 接入的会话没有独立的工作目录概念

Phase 4 改进：
1. **每个会话创建时自动分配独立工作目录**（可选）：
   ```rust
   pub enum SessionCwdMode {
       Inherit,                    // 继承全局 cwd（当前行为）
       Isolated { base_dir: PathBuf },  // 在 base_dir 下创建会话专属子目录
       Custom(PathBuf),            // 用户手动指定
   }
   ```

2. **Connector 会话工作目录**：Connector 创建的会话默认使用 `Isolated` 模式，在 `~/.tiangong/workspaces/{session_id}/` 下创建独立目录，避免不同 Connector 会话间相互干扰。

3. **Worker 隔离工作目录**：Phase 5 中多代理的每个 Worker 可在会话 cwd 下创建子目录（`{session_cwd}/.workers/{worker_id}/`），工具执行限制在子目录范围内。

4. **工具执行路径隔离**：`allowed_roots` 验证时以会话 cwd 为基准，而非全局 cwd。确保会话 A 的工具不能访问会话 B 的工作目录。

---

## Phase 5：多代理协调层 + 观测基础

**目标**：搭建完整的多代理协调层和观测基础设施，为多智能体协作奠定基础。

### 多智能体架构设计

```
                    ┌─────────────────┐
                    │  TaskCoordinator │  ← 接收复杂任务
                    │  (Coordinator)   │
                    └────────┬────────┘
                             │ 拆分子任务
              ┌──────────────┼──────────────┐
              ▼              ▼              ▼
        ┌─────────┐   ┌─────────┐   ┌─────────┐
        │ Worker A │   │ Worker B │   │ Worker C │
        │(TurnRunner)│ │(TurnRunner)│ │(TurnRunner)│
        └─────┬───┘   └─────┬───┘   └─────┬───┘
              │              │              │
              ▼              ▼              ▼
        ┌─────────┐   ┌─────────┐   ┌─────────┐
        │ 结果 A  │   │ 结果 B  │   │ 结果 C  │
        └─────┬───┘   └─────┬───┘   └─────┬───┘
              │              │              │
              └──────────────┼──────────────┘
                             ▼
                    ┌─────────────────┐
                    │  Coordinator    │  ← 汇总结果
                    │  最终答复/动作   │
                    └─────────────────┘
```

### Worker 独立边界

每个 Worker 是一个独立的 `TurnRunner` 实例，拥有：
```rust
pub struct WorkerContext {
    pub worker_id: String,
    pub task_objective: String,        // 子任务目标
    pub available_tools: Vec<String>,  // 可用工具子集
    pub context_scope: ContextScope,   // 可见上下文范围
    pub working_dir: Option<String>,   // 独立工作目录
    pub budget: WorkerBudget,          // Token/时间预算上限
    pub output_target: OutputTarget,   // 结果输出位置
}

pub struct WorkerBudget {
    pub max_tokens: usize,
    pub max_rounds: usize,
    pub max_duration_secs: u64,
}

pub enum ContextScope {
    Full,                              // 完整会话上下文
    TaskOnly,                          // 仅当前子任务上下文
    Isolated { initial_context: Vec<Message> },  // 完全隔离
}
```

### TaskCoordinator 实现

```rust
pub struct TaskCoordinator {
    engine: Arc<RuntimeEngine>,
    event_tx: mpsc::Sender<TurnEvent>,
}

impl TaskCoordinator {
    /// 判断是否需要拆分为多代理
    pub fn should_split(&self, task: &TaskPlan) -> bool {
        // 条件：子任务可并行、上下文差异大、需要隔离环境等
    }

    /// 拆分任务并分配给 Workers
    pub async fn coordinate(&self, task: CoordinatorTask) -> CoordinatorResult {
        let sub_tasks = self.split_task(&task);

        if sub_tasks.len() == 1 {
            // 单任务：直接用 TurnRunner 执行（退化为当前模式）
            return self.run_single(sub_tasks.into_iter().next().unwrap()).await;
        }

        // 多任务：并行执行
        let handles: Vec<_> = sub_tasks.into_iter().map(|sub| {
            let worker = self.create_worker(sub);
            tokio::spawn(async move { worker.run().await })
        }).collect();

        // 收集结果
        let results = futures::future::join_all(handles).await;

        // 汇总（可选：用 LLM 合成最终答复）
        self.merge_results(results)
    }
}
```

### 执行环境分级

```rust
pub enum ExecutionEnvironment {
    Foreground,          // 前台同步（当前默认）
    Background,          // 本地后台（已有 spawn_task 雏形）
    Isolated {           // 隔离环境（独立工作目录）
        work_dir: PathBuf,
    },
    // 未来扩展：
    // Sandboxed,        // 容器/沙箱
    // Remote { host },  // 远程主机
}
```

### 新增模块文件

**`crates/tiangong-core/src/coordinator/`**：
```
coordinator/
  mod.rs                   — 模块导出
  task_coordinator.rs      — 任务拆分、分配、汇总
  worker.rs                — Worker 定义，包装 TurnRunner
  types.rs                 — CoordinatorTask, WorkerContext, WorkerResult
```

**`crates/tiangong-core/src/observe/`**：
```
observe/
  mod.rs
  metrics.rs    — 延迟、错误率、工具执行统计
  cost.rs       — Token 成本追踪（请求级、任务级、会话级）
  audit.rs      — 审计记录（复用/扩展现有 AuditEntry）
```

### 新增 Tauri 命令
- `get_session_cost(session_id)` → 返回会话级成本统计
- `list_workers(session_id)` → 列出当前活跃的 Worker 状态

### 多智能体远景路线

Phase 5 建立的 Coordinator + Worker 架构，为后续多智能体能力提供基础：

| 能力 | Phase 5 状态 | 后续扩展 |
|------|-------------|---------|
| 单 Worker 执行 | ✅ 完整实现 | — |
| 多 Worker 并行 | ✅ 接口就绪 | 补充并行调度策略 |
| Worker 隔离工作目录 | ✅ 接口就绪 | 补充目录自动创建/清理 |
| Worker 预算控制 | ✅ 接口就绪 | 补充超预算自动终止 |
| 容器/沙箱执行 | ⬜ 接口预留 | 集成 Docker/Wasm |
| 远程 Worker | ⬜ 接口预留 | 通过 Server API 远程调度 |
| Worker 间通信 | ⬜ 未实现 | 共享消息总线 |
| 动态 Worker 扩缩 | ⬜ 未实现 | 根据任务负载自动调整 |

---

## 关键文件清单

| 阶段 | 新增文件 | 核心修改文件 |
|------|---------|------------|
| Phase 1 | `event.rs`, `turn_state.rs` | `runtime.rs`, `lib.rs` |
| Phase 2 | `permission.rs` | `runtime.rs`, `agent_config.rs` |
| Phase 3 | `turn_runner.rs` | `runtime.rs`(核心), `turn/start.rs`, `support.rs`, `turn_control.rs`, `commands.rs` |
| Phase 4 | `context/assembler.rs`, `context/budget.rs` | `session.rs`, `turn_runner.rs` |
| Phase 5 | `coordinator/`, `observe/` | `event.rs`, `turn_runner.rs`, `commands.rs` |

## 验证方式

每个阶段完成后：
1. `cargo check --workspace` 编译通过
2. `cargo clippy --workspace -- -D warnings` 无警告
3. `cargo nextest run --workspace --no-tests pass` 测试通过
4. 功能验证：发送消息 → LLM 响应 → 工具调用 → 流式输出，全链路正常
5. Phase 3 特别验证：对比新旧执行路径的 TurnEvent 序列一致性
