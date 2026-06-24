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

## 实施结果

- 新增 `ToolFailureRecord` 和 `ToolFailureKind`，字段覆盖：
  - `tool_name`
  - `tool_call_id`
  - `arguments_summary`
  - `error_kind`
  - `error_message`
  - `retryable`
  - `same_failure_count`
  - `recommended_next_action`
  - `requires_user_input`
- 常见失败分类已覆盖：
  - 参数错误：空参数、非法 JSON、`__parse_error`
  - 权限拒绝：权限门禁拒绝、文件写入锁拒绝
  - 用户拒绝：审批弹窗中用户拒绝
  - 命令失败：非 0 exit code 或 stderr
  - 超时：包含 timeout/超时
  - 环境缺失：command not found、路径不存在、依赖缺失
  - 网络失败：network/connection/DNS/网络/连接
  - 工具内部异常：兜底分类
- 失败写入会话时，模型可见 tool result 使用 `[tool_failure]` 结构化文本。
- `StreamEvent::ToolResult.output` 保持原有短文本，不要求前端新增结构化失败面板。
- 相同工具和相同参数重复失败时，`same_failure_count` 至少为 2，并明确要求不要重复同一调用。
- 失败恢复提示继续触发 Memory recall 优先策略；结构化记录为后续按失败类型决定是否 recall/index 查询提供入口。

## 恢复策略

| 失败类型 | retryable | requires_user_input | 建议动作 |
|---|---|---|---|
| `argument_error` | false | false | 修正 JSON/schema 参数；不要复用 `__parse_error` |
| `permission_denied` | false | true | 不重复执行，改安全方案或请求授权 |
| `user_rejected` | false | true | 尊重用户拒绝，切换无需该授权的方案 |
| `command_failed` | false | false | 阅读 stderr/stdout，修正命令、路径或环境 |
| `timeout` | true | false | 缩小范围、增加过滤或改轻量工具 |
| `environment_missing` | false | false | 确认路径、命令、依赖、工作目录；必要时询问用户 |
| `network_failure` | true | false | 检查网络、端点、凭据，可短暂重试 |
| `tool_internal_error` | true | false | 重新规划，避免盲目重复同一调用 |

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
