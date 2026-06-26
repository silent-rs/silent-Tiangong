# ReAct Loop 与总结阶段分离架构重构 — 总体设计

> 创建时间：2025-07-14
>
> 状态：设计定稿

## 完成标准

- ReAct Loop 只负责工具调用，不再在循环内输出最终回复
- 总结阶段由主模型执行，判断任务完成度并输出最终回复
- 移除 lite 模型完成度检测机制
- 前端能区分过程消息和最终回复，最终回复提供复制按钮
- 简单问答（无需工具）直接进入总结阶段，零额外开销
- Sub Agent 兼容新架构

## 设计原则

1. **职责分离**：工具执行（ReAct Loop）和总结判断（Summary Phase）是独立阶段
2. **主模型自决**：由主模型判断任务完成度，不引入弱模型监督
3. **显式标记 + 隐式兜底**：LLM 可输出标记主动触发阶段切换，系统也有隐式兜底
4. **向后兼容**：Session 持久化、命令通道、权限审批等不受影响
5. **最小前端改动**：通过新增 StreamEvent 类型实现消息分层，不重构前端消息系统

## 本阶段要做

- 重构 `execute_turn` 为两阶段循环（ToolExecution + Summary）
- 移除 `check_completion_with_lite_model` 及相关代码
- 新增阶段管理状态机和阶段切换逻辑
- 新增 StreamEvent 类型支持消息分层
- 调整 system prompt（ReAct Loop 阶段增量 + 总结阶段 prompt）
- 改进 `force_final_response` prompt
- 前端适配新的 StreamEvent 类型
- Sub Agent 兼容适配

## 本阶段不做

- 不改变工具执行机制（权限、审批、失败恢复）
- 不改变上下文压缩策略
- 不重构前端消息组件架构
- 不引入新的 LLM Provider
- 不改变 Session 持久化格式

## 架构设计

### 整体流程

```
用户消息
    │
    ▼
┌──────────────────────────────────────────────┐
│  外层循环 (max_outer_iterations = 3)          │
│                                              │
│  ┌────────────────────────────────────────┐  │
│  │  ReAct Loop (内层, max_rounds = 15)    │  │
│  │                                        │  │
│  │  LLM 请求 (带 tools)                   │  │
│  │    ├─ 有 tool_calls → 执行 → continue  │  │
│  │    ├─ 无 tool_calls → break (进入总结)  │  │
│  │    └─ round >= max → break (强制总结)  │  │
│  └────────────────────────────────────────┘  │
│                  │                           │
│                  ▼                           │
│  ┌────────────────────────────────────────┐  │
│  │  Summary Phase (不带 tools)            │  │
│  │                                        │  │
│  │  LLM 请求 (纯文本, 带 judgment prompt)  │  │
│  │    ├─ 任务完成 → 输出最终回复 → END    │  │
│  │    ├─ 需要用户输入 → 输出提问 → END    │  │
│  │    └─ 任务未完成 → 注入上下文 → 重入   │  │
│  └────────────────────────────────────────┘  │
│                                              │
│  outer_iteration >= max → 强制总结 → END     │
└──────────────────────────────────────────────┘
```

### 阶段定义

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnPhase {
    /// 首轮：主模型决定是否需要工具
    Initial,
    /// 工具执行阶段
    ToolExecution,
    /// 总结阶段
    Summary,
}
```

### ReAct Loop 阶段 — System Prompt 增量

在现有 system prompt 基础上，追加阶段指令：

```
你当前处于工具执行阶段。
- 专注于执行操作，调用需要的工具完成任务。
- 不要给出最终回复或长篇总结。
- 如果你认为所有必要的操作都已完成，或者需要用户提供额外信息才能继续，
  请停止调用工具，系统将自动进入总结阶段。
- 如果用户的问题不需要任何工具操作，直接停止即可。
```

关键设计：不再使用 `<<SUMMARIZE>>` 标记。LLM 只要不返回 tool_calls 就自然进入总结阶段。这更符合 LLM 的自然行为，减少 prompt 指令负担。

### 总结阶段 — Prompt 设计

```
你当前处于总结阶段。请基于以上所有工作，给出最终回复。

判断逻辑：
1. 如果用户请求的所有操作都已执行并得到结果，请总结结果给出最终回复。
2. 如果需要用户提供额外信息才能继续，请直接向用户提问。
3. 如果有关键步骤遗漏未执行，请在回复开头输出 [NEED_MORE_WORK]，
   然后简要说明还需要做什么。系统将重新进入工具执行阶段。

注意：不要重复执行工具调用。不要重复已有内容。
```

### 循环控制参数

| 参数 | 值 | 说明 |
|------|-----|------|
| `MAX_OUTER_ITERATIONS` | 3 | 总结→重入 Loop 的最大次数 |
| `MAX_TOOL_ROUNDS` | 15 | 每次 Loop 内的工具调用轮次上限 |
| `SUB_AGENT_MAX_TOOL_ROUNDS` | 8 | Sub Agent 每次 Loop 内的轮次上限 |
| `SUB_AGENT_MAX_OUTER_ITERATIONS` | 2 | Sub Agent 总结→重入的最大次数 |

### StreamEvent 新增

```rust
// 方案：在现有 StreamEvent 基础上新增
pub enum StreamEvent {
    // ... 现有事件保持不变 ...

    /// ReAct Loop 阶段的 LLM 文本输出（过程性）
    ReactText {
        message_id: String,
        content: String,
    },

    /// 总结阶段的最终回复（可复制）
    SummaryText {
        message_id: String,
        content: String,
    },

    /// 阶段切换通知
    PhaseChanged {
        phase: String,       // "tool_execution" / "summary"
        iteration: u32,      // 第几次外层循环
    },
}
```

前端处理：
- `ReactText` → 紧凑展示在"执行过程"区域，无复制按钮
- `SummaryText` → 作为主消息展示，有复制按钮
- `PhaseChanged` → 可选展示阶段状态

### 兜底机制

| 场景 | 兜底策略 |
|------|----------|
| LLM 在 Loop 中无 tool_calls | 隐式进入总结阶段 |
| 总结阶段无 `[NEED_MORE_WORK]` | 视为任务完成 |
| 外层循环超过上限 | 强制总结，`force_final_response` |
| 内层循环超过上限 | 强制进入总结阶段 |
| 总结阶段 LLM 错误 | 输出错误，使用已有上下文做 `force_final_response` |

### Session 消息存储

总结阶段的输出作为独立的 Assistant 消息存储，带有 `phase` 标记：

```rust
// Message 新增字段（向后兼容，默认 None）
pub phase: Option<MessagePhase>,

pub enum MessagePhase {
    React,    // ReAct Loop 中的消息
    Summary,  // 总结阶段的最终回复
}
```

## 兼容性策略

- 旧 Session 的消息 `phase` 字段为 `None`，前端按现有逻辑处理
- 新 Session 的 ReAct Loop 消息标记为 `React`，总结消息标记为 `Summary`
- `force_final_response` 产出的消息标记为 `Summary`

## 验收清单

- [ ] LLM 执行完任务后输出总结，不再出现瞎猜
- [ ] LLM 向用户提问后正常结束循环
- [ ] 简单问答不进入 ReAct Loop，直接输出回复
- [ ] 总结阶段判断任务未完成时，能正确重入 Loop
- [ ] 外层循环超过上限时，强制输出总结
- [ ] 前端能区分过程消息和最终回复
- [ ] 最终回复提供复制按钮，过程消息不提供
- [ ] Sub Agent 兼容新架构
- [ ] 取消/消息注入/审批等命令通道正常工作
- [ ] 上下文压缩正常工作
- [ ] 旧 Session 向后兼容

## 任务拆分入口

详见各任务 spec 文件：

- `task-01-turn-phase-state-machine.md` — 阶段状态机
- `task-02-react-loop-refactor.md` — ReAct Loop 重构
- `task-03-summary-phase.md` — 总结阶段实现
- `task-04-remove-lite-model-check.md` — 移除 lite 模型检测
- `task-05-stream-event-phase.md` — StreamEvent 阶段事件
- `task-06-session-message-phase.md` — Session 消息阶段标记
- `task-07-sub-agent-adaptation.md` — Sub Agent 适配
- `task-08-frontend-adaptation.md` — 前端适配
- `task-09-force-final-improvement.md` — force_final_response 改进
- `task-10-e2e-verification.md` — 端到端验收
