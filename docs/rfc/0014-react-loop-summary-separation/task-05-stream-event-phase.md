# 05 - StreamEvent 阶段事件与消息分层

## 目标

新增 StreamEvent 类型，支持前端区分 ReAct Loop 过程消息和总结阶段最终回复。

## 范围

- `crates/tiangong-types/src/stream.rs`（或对应 StreamEvent 定义文件）— 新增事件类型
- `crates/tiangong-core/src/react/engine.rs` — 在 ReAct Loop 和总结阶段发送新事件

## 依赖

- 前置任务：01, 02, 03
- 后续任务：08
- 可并行任务：04, 06, 09
- 阻塞说明：需要 Task 02 和 03 完成，才能在正确的位置发送新事件

## 任务

- [ ] 新增 StreamEvent 变体：
  ```rust
  /// ReAct Loop 阶段的 LLM 文本输出（过程性，前端不提供复制）
  ReactText {
      message_id: String,
      content: String,
  },

  /// 总结阶段的最终回复（前端提供复制按钮）
  SummaryText {
      message_id: String,
      content: String,
  },

  /// 阶段切换通知
  PhaseChanged {
      phase: String,       // "tool_execution" / "summary"
      iteration: u32,      // 第几次外层循环
  },
  ```
- [ ] 在 `execute_turn` 中发送 `PhaseChanged` 事件：
  - 进入 ToolExecution 阶段时发送 `phase: "tool_execution"`
  - 进入 Summary 阶段时发送 `phase: "summary"`
- [ ] 在 ReAct Loop 中，LLM 的文本输出改为发送 `ReactText`（替代部分 `Delta`）
  - 注意：工具调用相关的 `Delta` 保持不变，只有 LLM 的"过程性文本"改为 `ReactText`
  - 如果难以区分，可以简化为：ReAct Loop 内的所有 `Delta` 改为 `ReactText`
- [ ] 在总结阶段，LLM 的文本输出发送 `SummaryText`（替代 `Delta`）
- [ ] Sub Agent 的 stream forwarder 适配新事件类型
- [ ] 确保新事件在 `spawn_sub_agent_stream_forwarder` 中正确转发或转换

## 不做

- 不移除 `Delta` 事件（保持向后兼容，其他模块可能使用）
- 不修改前端（Task 08）
- 不修改 `ToolResult` / `ToolStart` 等工具相关事件

## 验收

- `ReactText`、`SummaryText`、`PhaseChanged` 三个新事件类型已定义
- ReAct Loop 中的 LLM 文本输出发送 `ReactText`
- 总结阶段的 LLM 文本输出发送 `SummaryText`
- 阶段切换时发送 `PhaseChanged`
- 现有的 `Delta`、`ToolResult`、`ToolStart` 等事件不受影响
- Sub Agent 的事件转发正常
- `cargo check` 全项目通过

## 验证

```bash
cargo check
# 手动验证：观察前端事件流，确认新事件类型正确发送
```
