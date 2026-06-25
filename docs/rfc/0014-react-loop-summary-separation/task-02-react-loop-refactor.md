# 02 - ReAct Loop 重构为纯工具执行阶段

## 目标

将现有 ReAct Loop 内部逻辑重构为纯工具执行阶段：LLM 只负责调用工具，不再在 Loop 内输出最终回复。

## 范围

- `crates/tiangong-core/src/react/engine.rs` — 重构 `'react_loop` 内部逻辑
- `crates/tiangong-core/src/react/context.rs` — 调整 `rebuild_system_prompt` 支持阶段增量

## 依赖

- 前置任务：01
- 后续任务：03, 04, 05
- 可并行任务：06
- 阻塞说明：需要 Task 01 的 `TurnPhase` 枚举和外层循环骨架

## 任务

- [ ] 将现有 `'react_loop` 逻辑移入外层循环的 `ToolExecution` 分支
- [ ] 移除 Loop 内的"无 tool_calls 时输出最终回复"逻辑（改为 break 出 Loop 进入总结）
- [ ] 保留 Loop 内的所有工具执行逻辑：
  - 工具权限检查、审批流程
  - 工具执行、结果追加
  - 失败恢复提示
  - 重复调用检测
  - 浏览器状态注入
  - 记忆候选评估
  - 上下文压缩
- [ ] 保留 Loop 内的命令通道处理（cancel/message injection/cwd 等）
- [ ] 保留 Sub Agent drain 逻辑（但 Sub Agent 的 `execute_turn` 适配在 Task 07 完成）
- [ ] 当 LLM 无 tool_calls 时，保存 LLM 文本到 session 并 break 出内层 Loop
- [ ] 当 `round >= MAX_TOOL_ROUNDS` 时，break 出内层 Loop
- [ ] 在 ReAct Loop 阶段注入 system prompt 增量段：
  ```
  你当前处于工具执行阶段。
  - 专注于执行操作，调用需要的工具完成任务。
  - 不要给出最终回复或长篇总结。
  - 如果你认为所有必要的操作都已完成，或者需要用户提供额外信息才能继续，
    请停止调用工具，系统将自动进入总结阶段。
  - 如果用户的问题不需要任何工具操作，直接停止即可。
  ```
- [ ] `is_synthetic_tool_call_placeholder` 检查保留，但不再 continue，改为 break 进入总结

## 不做

- 不实现总结阶段逻辑（Task 03）
- 不移除 lite 模型检测代码（Task 04，本任务先注释或跳过，保持编译通过）
- 不修改 StreamEvent（Task 05）
- 不修改 Sub Agent 的 execute_turn（Task 07）

## 验收

- ReAct Loop 中 LLM 无 tool_calls 时，不再输出最终回复，而是 break 进入总结阶段占位
- 所有工具执行逻辑（权限、审批、失败恢复、重复检测）保持正常
- 命令通道（cancel/message injection）在 Loop 内正常工作
- 上下文压缩在 Loop 内正常工作
- `cargo check` 通过

## 验证

```bash
cargo check -p tiangong-core
# 手动验证：启动应用，发送需要工具的消息，观察 LLM 是否只做工具调用不再输出最终回复
```
