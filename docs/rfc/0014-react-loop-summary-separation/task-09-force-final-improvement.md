# 09 - force_final_response 改进

## 目标

改进 `force_final_response` 的 prompt 和行为，使其在超限场景下能输出更高质量的最终回复，并适配新的两阶段架构。

## 范围

- `crates/tiangong-core/src/react/context.rs` — 改进 `force_final_response` 函数

## 依赖

- 前置任务：01, 03
- 后续任务：10
- 可并行任务：04, 05, 06, 07, 08
- 阻塞说明：需要 Task 01 的循环控制常量和 Task 03 的总结阶段逻辑

## 任务

- [ ] 改进 `force_final_response` 的 system-reminder prompt：
  ```
  改进前：
  "请基于以上所有工具执行结果，直接给出最终回复。"

  改进后：
  "已达到最大执行轮次。请基于以上所有工作，给出最终回复。
  要求：
  1. 总结已完成的操作和结果。
  2. 如果有未完成的任务，说明原因和后续建议。
  3. 不要重复执行工具调用。
  4. 如果需要用户提供信息才能继续，请明确列出需要什么。"
  ```
- [ ] `force_final_response` 产出的消息标记 `phase: MessagePhase::Summary`（依赖 Task 06）
- [ ] `force_final_response` 的流式输出发送 `SummaryText` 事件（依赖 Task 05）
- [ ] 区分两种超限场景的 prompt：
  - **内层 Loop 超限**（`round >= MAX_TOOL_ROUNDS`）：工具调用轮次用尽
    ```
    "工具执行轮次已达上限。请基于已完成的操作给出最终回复。"
    ```
  - **外层循环超限**（`outer_iteration >= MAX_OUTER_ITERATIONS`）：总结→重入次数用尽
    ```
    "任务已经过多轮迭代仍未完全完成。请基于以上所有工作给出最终回复，
    说明已完成的部分和未完成的原因。"
    ```
- [ ] 确保 `force_final_response` 在新架构中被正确调用：
  - 内层 Loop 超限时 → 进入总结阶段（而非直接 force_final_response）
  - 外层循环超限时 → 调用 `force_final_response`
  - 总结阶段 LLM 错误时 → 调用 `force_final_response` 作为兜底

## 不做

- 不改变 `force_final_response` 的 LLM 请求方式（仍使用主模型）
- 不改变 `force_final_response` 的流式输出机制
- 不改变 `force_final_response` 的 token usage 上报

## 验收

- `force_final_response` 的 prompt 已改进
- 产出的消息标记为 `Summary` phase
- 内层和外层超限有不同的提示语
- `cargo check` 通过

## 验证

```bash
cargo check -p tiangong-core
# 手动验证：构造需要大量工具调用的任务，观察超限时的最终回复质量
```
