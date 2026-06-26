# 07 - Sub Agent 适配新架构

## 目标

确保 Sub Agent 的 `execute_turn` 调用兼容新的两阶段循环架构，Sub Agent 也能正确进入总结阶段并返回结果。

## 范围

- `crates/tiangong-core/src/react/engine.rs` — Sub Agent 的 ReactEngine 构建和执行逻辑
- `crates/tiangong-core/src/agent_team/tools.rs` — Sub Agent 循环控制常量

## 依赖

- 前置任务：01, 02, 03
- 后续任务：10
- 可并行任务：04, 05, 06, 08, 09
- 阻塞说明：需要 Task 02 和 03 完成，execute_turn 的两阶段循环已可用

## 任务

- [ ] 更新 `SUB_AGENT_MAX_ROUNDS` 为 `SUB_AGENT_MAX_TOOL_ROUNDS = 8`
- [ ] 新增 `SUB_AGENT_MAX_OUTER_ITERATIONS = 2`
- [ ] 在 `spawn_ready_sub_agents` 中，构建 Sub Agent 的 ReactEngine 时使用新的循环控制常量
- [ ] 确认 Sub Agent 的 `execute_turn` 能正确走完两阶段循环：
  - Sub Agent 进入 ToolExecution → 执行工具 → 进入 Summary → 输出总结
  - Sub Agent 的总结消息正确回传给主 Agent
- [ ] Sub Agent 的总结阶段 system prompt 需要适配 Sub Agent 角色：
  - 在总结 prompt 中加入 Sub Agent 的角色上下文
  - 例如："你是 {agent_label}，请基于以上工作向主 Agent 汇报结果"
- [ ] Sub Agent 的 `PhaseChanged` 事件在 `spawn_sub_agent_stream_forwarder` 中正确转发
- [ ] Sub Agent 的 `ReactText` / `SummaryText` 事件正确转发或转换为 `AgentOutput`
- [ ] 确认 Sub Agent 重入 Loop（`[NEED_MORE_WORK]`）时的上下文注入不会导致无限循环

## 不做

- 不修改 Sub Agent 的创建/解散机制
- 不修改 Sub Agent 的消息路由机制
- 不修改 Sub Agent 的权限/工具过滤机制
- 不修改前端 Sub Agent 展示

## 验收

- Sub Agent 能正确进入总结阶段并输出总结回复
- Sub Agent 的总结回复正确回传给主 Agent（通过 `deliver_main_message`）
- Sub Agent 的 `PhaseChanged` 事件正确转发到前端
- Sub Agent 不会因新架构导致无限循环
- `cargo check` 通过

## 验证

```bash
cargo check -p tiangong-core
# 手动验证：
# 1. 创建 Sub Agent 并分配任务
# 2. 观察 Sub Agent 是否正确进入总结阶段
# 3. 观察主 Agent 是否收到 Sub Agent 的总结回复
```
