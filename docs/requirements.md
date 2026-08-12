# 天工需求整理

## OpenAI Responses 适配

### 必须满足

- 同时支持 Responses API（`/responses`，协议值 `openai`）和 Chat Completions（`/chat/completions`，协议值 `openai_chatcompletions`）。
- 未配置协议及旧别名 `openai_compatible` 继续使用 Chat Completions，避免第三方端点误用 Responses。
- Responses 支持同步与流式文本、思考摘要、工具调用、工具结果回放、token 用量和结束原因。
- 多工具调用保持身份和顺序；缺失参数增量时从完成事件补齐参数。
- 工具结果通过原 `call_id` 映射为后续请求的 `function_call_output`。
- Responses 原始结构不得泄漏到 Core、Session 或前端消息模型。
- Chat Completions、Anthropic 和 DeepSeek 的已有行为不得回退。
- 前端供应商配置必须提供 OpenAI Responses 选项，并明确区分 Responses 与 Chat Completions，保存值分别为 `openai` 和 `openai_chatcompletions`。
- OpenAI Responses 流式请求必须使用普通流式模式，不得启用 `background`；本地取消时必须终止模型任务并关闭流式连接。

### 完成标准

- `tiangong-llm` 格式、编译、测试和严格检查通过。
- 直接依赖模型层的核心模块能够编译。
- 改动仅包含 Responses 适配及对应规划和需求记录。
