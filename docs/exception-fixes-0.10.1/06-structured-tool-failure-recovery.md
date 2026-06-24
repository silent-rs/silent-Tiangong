# 06 - 工具失败恢复结构化

## 目标

将工具失败恢复从普通文本提示升级为结构化运行时上下文，减少模型盲目重试并提升失败诊断能力。

## 范围

- `crates/tiangong-core/src/react/engine.rs`
- `crates/tiangong-core/src/react/message.rs`
- 工具结果消息结构和运行时工具消息格式
- `StreamEvent::ToolResult` 或相关事件，如需要扩展
- 与 Memory recall 触发策略相关的轻量接入点

## 依赖

- 前置任务：03、04
- 后续任务：07
- 可并行任务：05
- 阻塞说明：03 未完成前，空参数失败还未稳定；04 未完成前，主循环阶段边界不清，不适合加入结构化策略。

## 任务

- 定义工具失败结构字段，例如：
  - `tool_name`
  - `tool_call_id`
  - `arguments_summary`
  - `error_kind`
  - `error_message`
  - `retryable`
  - `same_failure_count`
  - `recommended_next_action`
  - `requires_user_input`
- 将常见失败分类：参数错误、权限拒绝、命令失败、超时、环境缺失、网络失败、工具内部异常。
- 在失败写入会话时保留结构化信息，并转换为模型可读提示。
- 对重复失败加入去重和停止条件。
- 明确哪些失败应该触发 Memory recall 或 workspace index 查询。
- 补充测试或手动验证场景。

## 不做

- 不实现只读工具并行。
- 不改变工具正常成功结果格式。
- 不修改所有工具实现。
- 不要求前端立即展示完整结构化失败面板。

## 验收

- 工具失败后，模型能看到明确的失败类型、是否可重试和建议下一步。
- 同一工具同一参数重复失败时，不会无限盲目重试。
- 权限拒绝、参数错误、命令失败等常见场景有不同恢复提示。
- 结构化失败信息可被日志或测试观察。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
- 构造参数错误、权限拒绝、命令失败三个场景，确认恢复提示不同。
- 如有前端事件变更，运行 `yarn --cwd frontend build`。
