# 04 - ReAct 主循环阶段化重构设计

> 历史文档：本文形成于旧版 React Loop 拆分阶段，涉及的目录、阶段和 Sub Agent
> 收尾流程已不代表当前实现。仅作为 0.10.1 异常修复历史保留；当前架构请以
> `docs/core-architecture.md` 和 `docs/agent-loop-refactor/design.md` 为准。

## 目标

为 `ReactEngine::execute_turn` 的后续安全重构产出设计文档和任务边界，降低当前主循环职责过重带来的维护风险。

## 范围

- `crates/tiangong-core/src/react/engine.rs`
- `crates/tiangong-core/src/react/context.rs`
- `crates/tiangong-core/src/react/message.rs`
- 新增或更新 ReAct 执行设计文档
- 必要的任务 spec 拆分

## 依赖

- 前置任务：01、03
- 后续任务：06、07
- 可并行任务：05
- 阻塞说明：03 未完成前，不应设计依赖旧失败恢复行为的阶段边界。

## 任务

- 梳理当前 `execute_turn` 的职责边界。
- 明确阶段化目标：行为不变、结构拆分、可测试性提升。
- 设计候选阶段：
  - `prepare_turn`
  - `prepare_round`
  - `drain_commands`
  - `inject_runtime_context`
  - `build_model_request`
  - `run_model_stream`
  - `handle_model_response`
  - `execute_tool_calls`
  - `handle_failure_recovery`
  - `finalize_turn`
- 标注每个阶段输入、输出、副作用和错误处理方式。
- 明确哪些逻辑本阶段只设计不实现。
- 拆出后续可独立执行的重构任务 spec。

## 当前职责边界

`ReactEngine::execute_turn` 当前同时承担以下职责：

| 职责 | 当前位置/现象 | 拆分风险 |
|---|---|---|
| Turn 状态初始化 | 初始化 round、usage、工具去重、失败记录、浏览器快照、Memory 计数等循环状态 | 状态字段分散，后续新增恢复策略容易遗漏重置点 |
| 首轮上下文准备 | 首轮重建 system prompt、注入浏览器当前状态、处理用户 mention 路由到 Sub Agent | 首轮逻辑和每轮逻辑混在一起，难以单独验证 |
| 命令排空 | 多处调用 `drain_pending_commands_async`，并在模型流式、工具执行、工具执行后重复处理命令 | 重置状态和继续循环的语义重复，容易行为漂移 |
| 模型请求构建 | 复制工具列表、构建 thinking 配置、创建 `ModelRequest`、选择 client | 请求构建和执行耦合，不利于测试不同模型协议 |
| 流式模型执行 | 创建 chunk 通道、处理取消策略、推送 delta/reasoning、累计 streaming usage | 取消策略、usage 上报和错误恢复混在一起 |
| 模型响应处理 | 累计 usage、写 LLM 输出、处理空工具调用、完成度检测、保存 assistant 消息 | 普通回复和工具调用分支过长，后续改动容易影响最终回复 |
| 工具调用写入 | 发送 `ToolCalls`、保存 assistant tool_call 消息、记录 runtime trace | 与工具执行循环耦合，难以单测消息格式 |
| 工具执行 | 参数解析错误、团队工具、权限审核、审批等待、文件锁、实际工具调用、结果写回、Memory 候选、浏览器更新 | 这是当前最大职责块，也是 06 和 07 的主要依赖 |
| 失败恢复 | 记录失败 key/name，重复失败跳过，必要时注入 `react_failed_tool_recovery` 提示并继续循环 | 03 已稳定空参数错误，但失败类型仍是文本化 |
| 子 Agent 收尾 | 执行待处理 Sub Agent inbox、接收主 Agent 消息、决定 Done 或继续循环 | 与主 Agent 工具结果收尾混在一起，后续协作策略难验证 |

## 阶段化目标

- 行为不变：先只迁移代码位置和状态传递，不改变用户可见事件、消息顺序、权限语义、Memory 语义和模型请求协议。
- 状态显式：把循环内跨阶段共享的变量收敛为 `TurnState`/`RoundState`，避免每个 helper 手动传入十多个可变参数。
- 副作用可见：每个阶段明确它会修改 `Session`、发送 `StreamEvent`、持久化、调用外部工具或触发模型请求。
- 可测试：纯构造逻辑优先拆成同步小函数；有 IO/异步的阶段通过最小集成测试或手动模拟验证。
- 可回滚：每个后续重构任务只移动一个阶段或一组强相关阶段，不和 06/07 的行为增强混在一起。

## 候选阶段设计

| 阶段 | 输入 | 输出 | 副作用 | 错误/中断处理 |
|---|---|---|---|---|
| `prepare_turn` | `session`、`user_input`、`stream_tx`、团队上下文 | `TurnState`，或直接 `Done` | main Agent mention 路由、Sub Agent inbox drain、session 持久化、Done 事件 | 用户 mention 已路由时结束当前 turn；Sub Agent 取消时返回已累计 usage |
| `prepare_round` | `TurnState`、`session`、`cmd_rx` | `RoundControl`：继续、结束、重试 | 首轮重建 system prompt；排空命令；首轮浏览器状态注入；最大轮次触发强制最终回复 | Cancel/Shutdown 返回当前 usage；MessageInjected 清空工具去重、失败、Memory 计数 |
| `drain_commands` | `session`、`cmd_rx`、当前阶段枚举 | `PendingCommandEffect` 加状态重置信号 | 写入新用户消息、更新 cwd、注入工具结果、压缩/重置上下文 | 统一处理 Cancel/Shutdown；不同调用点只决定继续当前阶段还是回到 react loop |
| `inject_runtime_context` | `session`、浏览器快照状态、阶段标记 | 更新后的浏览器快照状态 | 注入浏览器页面变化、发送相关 StreamEvent、写入工具消息 | 注入失败不应终止 turn；只记录 warning 或普通工具反馈 |
| `build_model_request` | `session`、`TurnState`、engine 配置 | `ModelRequest`、工具列表、pending message id | 生成 scru128 message id；不写 session | 构建失败应作为模型请求错误处理，当前阶段不引入新失败路径 |
| `run_model_stream` | `ModelRequest`、工具列表、client、`cmd_rx` | `ModelFunctionResponse`、streaming usage、是否流式中注入了用户消息 | 启动模型任务、推送 delta/reasoning、累计 usage、处理流式期间命令 | 按协议区分 AbortWithStreamingUsage/WaitForUsage；上下文超限交给下一阶段决定是否压缩重试 |
| `handle_model_response` | `response`、`TurnState`、`pending_msg_id` | `ModelResponseAction`：Final、ToolCalls、RetryRound | usage 上报、完成度检查、保存 assistant 消息、写 runtime LLM 输出、上下文压缩 | 空响应占位继续下一轮；完成度不足注入 reminder 并继续；普通错误返回 Error 事件 |
| `execute_tool_calls` | tool calls、`TurnState`、runtime/memory/index/team 上下文 | `ToolExecutionSummary` | 发送 ToolCalls/ToolStart/ToolResult；权限审核和审批；执行工具；写 tool result/runtime trace；Memory 候选；浏览器更新；上下文压缩 | 参数解析错误只写失败结果不执行真实工具；拒绝审批沿用当前立即 Done 行为；重复成功/失败跳过 |
| `handle_failure_recovery` | `ToolExecutionSummary`、失败集合、可用工具列表 | `RoundControl` | 注入 `react_failed_tool_recovery`；必要时重置 memory recall 标记；session 持久化 | 有失败则继续 loop；无失败进入 Sub Agent 收尾 |
| `finalize_turn` | `TurnState`、Sub Agent 状态、main inbox | `TokenUsage` 或继续 loop | drain Sub Agent inbox、写入 main Agent 消息、session 持久化、Done 事件 | Sub Agent 取消返回当前 usage；有主 Agent 消息则继续 loop |

## 建议数据结构

| 结构 | 负责字段 | 说明 |
|---|---|---|
| `TurnState` | `round`、`accumulated_usage`、`memory_recall_attempted`、`successful_tool_call_keys`、`failed_tool_call_keys`、`failed_tool_names`、`memory_candidate_count`、`completion_check_count`、浏览器快照状态 | 存活于整个 `execute_turn`，替代当前函数顶部的一组局部变量 |
| `RoundModelOutput` | `pending_msg_id`、`response`、`streaming_usage`、`user_message_injected_during_stream` | 模型阶段到响应处理阶段的边界对象 |
| `ToolExecutionSummary` | `failed`、`failed_tool_names`、`ran_team_tool`、`ran_regular_tool`、`cancelled`、`message_injected` | 工具执行阶段的结果摘要；06 可在此扩展结构化失败 |
| `RoundControl` | `ContinueLoop`、`Return(TokenUsage)`、`Break`、`RetryAfterCompression` | 替代多处 `continue 'react_loop` 和提前 return，让控制流可测试 |

## 不变行为清单

- 首轮仍必须重建 system prompt。
- 多轮 ReAct 请求仍不能再次追加原始 `user_input`。
- 模型流式生成仍支持用户取消；不同协议的取消 usage 策略保持不变。
- 上下文超限或 Anthropic 空 content 场景仍先尝试压缩，压缩成功后重试。
- 空参数和非法 JSON 工具调用仍只作为错误结果写回，不进入真实工具执行。
- 相同工具和相同参数的重复成功调用仍跳过；重复失败调用仍提示模型重新规划。
- 权限审核、用户审批、拒绝后的返回行为保持当前语义。
- `recall_memory` 单轮重复调用仍使用重复结果，不改变 Memory 主动召回策略。
- 文件写入锁、媒体工具即时 assistant 消息、Memory 候选评估、浏览器变化注入保持现有顺序。
- `StreamEvent::ToolCalls`、`ToolStart`、`ToolResult`、`TokenUsage`、`Done` 的用户可见顺序保持不变。
- Sub Agent inbox drain 和 main Agent 消息注入保持现有时机。

## 后续任务拆分

| 子任务 | 目标 | 主要改动 | 验证 |
|---|---|---|---|
| 04-A 提取 Turn/Round 状态 | 新增内部状态结构，替换局部变量传递 | 只改 `react/engine.rs` 内部结构，不移动业务逻辑 | `cargo fmt -- --check`、`cargo check --workspace` |
| 04-B 统一命令排空结果 | 将命令处理的状态重置和控制流收敛到单一 helper | 减少模型流式、工具前、工具后三处重复分支 | 构造 MessageInjected、Cancel、UpdateCwd 手动场景 |
| 04-C 提取模型请求与流式执行 | 拆出 `build_model_request` 与 `run_model_stream` | 不改模型协议、不改 stream event 内容 | 模拟普通回复、工具调用、取消、上下文超限 |
| 04-D 提取模型响应分流 | 拆出最终回复、完成度检查、工具调用消息写入 | 保持 assistant 消息、runtime trace、TokenUsage 顺序 | 普通回复、完成度不足、tool_calls 三类场景 |
| 04-E 提取串行工具执行阶段 | 将工具执行循环移动到阶段函数 | 保持串行执行、权限审核、文件锁、Memory 候选、浏览器更新顺序 | 参数错误、权限拒绝、工具成功/失败、重复失败 |
| 04-F 提取失败恢复和收尾 | 拆出失败 reminder、Sub Agent drain、Done/continue 决策 | 为 06/07 提供清晰接入点 | 工具失败恢复、Sub Agent 有/无任务、main inbox 消息 |

## 与 06、07 的关系

- 06 工具失败恢复结构化应基于 `ToolExecutionSummary` 扩展，不直接在工具执行循环里散落新增字段。
- 06 可以先定义 `ToolFailureRecord`，由 `execute_tool_calls` 产生，由 `handle_failure_recovery` 转换为模型可读提示。
- 07 只读工具并行执行只能替换 `execute_tool_calls` 内部调度；阶段边界外的模型响应写入、失败恢复、Sub Agent 收尾不应同时改。
- 07 的并行结果必须仍按模型 tool_call 顺序写回 session，除非后续 spec 明确变更。

## 本任务结论

本任务只完成设计，不修改 `execute_turn` 行为。后续重构应按 04-A 到 04-F 顺序推进；06、07 不应绕过这些阶段边界直接扩展当前大函数。

## 不做

- 本任务不直接大规模修改 `execute_turn` 行为。
- 不实现工具并行。
- 不改变 Memory 主动召回策略。
- 不改变权限审核语义。
- 不改变模型请求协议。

## 验收

- 有明确设计文档说明 ReAct 主循环如何阶段化。
- 有后续重构任务拆分，每个任务可独立验证。
- 设计中列出不变行为清单，避免重构引入行为漂移。
- 设计中标注和 06、07 的依赖关系。

## 验证

- 手动审查设计文档。
- 如仅文档变更，可不运行编译命令。
- 如果附带轻微代码整理，运行 `cargo fmt -- --check` 与 `cargo check --workspace`。

## GitHub Issue

- Issue: #167
- PR 关闭关键字：`Closes #167`
