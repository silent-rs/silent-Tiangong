# 03 - 总结阶段实现

## 目标

实现独立的总结阶段：主模型基于所有工具执行结果，判断任务完成度并输出最终回复，或在任务未完成时重新进入 ReAct Loop。

## 范围

- `crates/tiangong-core/src/react/engine.rs` — 在外层循环中实现 Summary 分支
- `crates/tiangong-core/src/react/context.rs` — 新增总结阶段 prompt 构建

## 依赖

- 前置任务：01, 02
- 后续任务：05, 07, 09
- 可并行任务：04, 06
- 阻塞说明：需要 Task 02 的 ReAct Loop 重构完成，Loop break 后能进入总结阶段

## 任务

- [ ] 实现总结阶段的 LLM 请求构建：
  - 不携带 tools（纯文本生成）
  - 携带总结阶段 system prompt 增量
  - 使用主模型 client（非 lite client）
  - 支持流式输出
- [ ] 实现总结阶段 system prompt：
  ```
  你当前处于总结阶段。请基于以上所有工作，给出最终回复。

  判断逻辑：
  1. 如果用户请求的所有操作都已执行并得到结果，请总结结果给出最终回复。
  2. 如果需要用户提供额外信息才能继续，请直接向用户提问。
  3. 如果有关键步骤遗漏未执行，请在回复开头输出 [NEED_MORE_WORK]，
     然后简要说明还需要做什么。系统将重新进入工具执行阶段。

  注意：不要重复执行工具调用。不要重复已有内容。
  ```
- [ ] 实现总结阶段响应解析：
  - 检测 `[NEED_MORE_WORK]` 标记
  - 有标记且 `outer_iteration < MAX_OUTER_ITERATIONS` → 提取"还需要做什么"作为上下文注入，`continue 'outer`
  - 无标记 → 任务完成，输出最终回复
- [ ] 实现总结阶段的流式输出：
  - 使用 `ThrottledStreamSink` 流式输出
  - 发送 `StreamEvent::Delta`（Task 05 会改为 `SummaryText`）
  - 总结完成后发送 `StreamEvent::Done`
- [ ] 实现总结阶段的消息存储：
  - 总结回复作为独立的 Assistant 消息追加到 session
  - 标记 `phase: Some(MessagePhase::Summary)`（依赖 Task 06）
- [ ] 实现总结阶段的 token usage 上报
- [ ] 实现总结阶段的上下文压缩触发
- [ ] 实现总结阶段的命令通道处理（cancel/message injection）
- [ ] 重入 Loop 时的上下文注入：
  - 将上次总结的"还需要做什么"作为 Tool 消息注入 session
  - 格式：`<system-reminder>上轮总结判定任务未完成，原因：{reason}。请继续执行。</system-reminder>`

## 不做

- 不移除 lite 模型检测代码（Task 04）
- 不修改 StreamEvent 类型（Task 05，本任务先用现有 `Delta`）
- 不修改前端
- 不改进 `force_final_response`（Task 09）

## 验收

- ReAct Loop break 后，正确进入总结阶段
- 总结阶段使用主模型（非 lite 模型）生成回复
- 总结阶段流式输出正常
- 任务完成时，输出最终回复并结束 `execute_turn`
- 任务未完成时，输出 `[NEED_MORE_WORK]` 并重新进入 ReAct Loop
- 向用户提问时（无 `[NEED_MORE_WORK]`），正常结束
- 外层循环超过上限时，进入 `force_final_response`
- `cargo check` 通过

## 验证

```bash
cargo check -p tiangong-core
# 手动验证：
# 1. 发送需要工具的消息（如"列出当前目录文件"），观察是否有独立的总结回复
# 2. 发送简单问答（如"你好"），观察是否直接进入总结阶段
# 3. 发送需要多步工具的消息，观察总结后是否正确判断完成度
```
