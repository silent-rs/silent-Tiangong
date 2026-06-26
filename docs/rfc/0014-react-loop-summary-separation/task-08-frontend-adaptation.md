# 08 - 前端适配消息分层展示

## 目标

前端适配新的 StreamEvent 类型，实现 ReAct Loop 过程消息和总结阶段最终回复的分层展示，最终回复提供复制按钮，过程消息不提供。

## 范围

- `frontend/src/` — 消息渲染组件、事件处理逻辑
- `src-tauri/src/` — Tauri 事件桥接（如有需要）

## 依赖

- 前置任务：05, 06
- 后续任务：10
- 可并行任务：04, 07, 09
- 阻塞说明：需要 Task 05 的 StreamEvent 新类型和 Task 06 的 Message phase 字段

## 任务

- [ ] 在前端事件处理中新增对 `ReactText`、`SummaryText`、`PhaseChanged` 的处理
- [ ] 消息渲染分层：
  - `ReactText` / `phase: "react"` 的消息：紧凑展示在"执行过程"区域
    - 不提供复制按钮
    - 可选：默认折叠或紧凑样式
  - `SummaryText` / `phase: "summary"` 的消息：作为主消息展示
    - 提供复制按钮
    - 正常 Markdown 渲染
  - `phase: "normal"` 的消息（旧消息）：按现有逻辑展示，提供复制按钮
- [ ] `PhaseChanged` 事件处理：
  - 可选：在消息区域顶部显示阶段状态指示器
  - "正在执行..."（tool_execution）→ "正在总结..."（summary）
- [ ] 确保流式输出连续性：
  - `ReactText` 的流式增量正确追加到对应消息
  - `SummaryText` 的流式增量正确追加到对应消息
  - 阶段切换时不会出现消息闪烁或重置
- [ ] 兼容旧消息：`phase` 为 `Normal` 或缺失时，按现有逻辑展示

## 不做

- 不重构前端消息组件架构
- 不改变 Markdown 渲染引擎
- 不改变消息列表的滚动行为
- 不改变消息的持久化逻辑

## 验收

- ReAct Loop 中的过程消息不显示复制按钮
- 总结阶段的最终回复显示复制按钮
- 阶段切换时消息流不中断
- 旧消息（`phase: Normal`）展示正常，有复制按钮
- 流式输出无闪烁

## 验证

```bash
# 前端构建
cd frontend && npm run build
# 手动验证：
# 1. 发送需要工具的消息，观察过程消息无复制按钮，最终回复有复制按钮
# 2. 发送简单问答，观察直接显示最终回复且有复制按钮
# 3. 加载旧 Session，观察旧消息展示正常
```
