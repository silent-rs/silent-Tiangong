# 天工项目规划（PLAN）

## 总体目标

构建可扩展的个人 AI 中枢，保持桌面端、命令行和服务端共享统一的 Agent 与模型能力。

## 当前里程碑

### 0.14.x：模型协议适配完善

- 恢复并完善 OpenAI Responses API 支持。
- 保留 OpenAI Chat Completions 作为第三方兼容端点的默认协议。
- 统一文本、思考摘要、工具调用、用量和结束原因的上层行为。

## 优先级与架构边界

1. Responses 文本、流式响应和工具调用稳定可用。
2. 协议细节只存在于 `tiangong-llm` Provider 适配层。
3. `openai` 对应 Responses；`openai_chatcompletions` 和旧别名 `openai_compatible` 对应 Chat Completions。
4. 不影响 Chat Completions、Anthropic 和 DeepSeek。

## 关键时间节点

- 2026-08-12：基于 0.14.1 主线完成 Responses 适配和验证。
