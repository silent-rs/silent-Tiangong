# 04 - ReAct 主循环阶段化重构设计

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
