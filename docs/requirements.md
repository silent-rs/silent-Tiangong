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
- OpenAI Responses 与 Chat Completions 的工具调用必须逐项校验；同一响应中存在异常调用时，合法调用仍须正常执行，异常调用必须从执行列表剔除。
- 流式工具参数只能在能够解析为完整 JSON 时视为完整，不得仅凭首尾括号判断并丢弃后续参数增量。
- 工具名称不存在、参数 JSON 无效或参数不符合本次定义时，必须将调用编号、工具名、原参数和 schema 校验原因作为内部纠错信息交给下一轮模型；不得自动重发整轮请求，也不得生成固定错误文字结束对话。
- 同一响应中的工具调用全部异常时，必须通过现有有界对话循环携带校验原因继续生成，不得静默结束或无限重试。
- 模型响应中的 token 用量必须保持单次请求语义，不得把参数修正等额外请求累计成当前上下文大小。

### 完成标准

- `tiangong-llm` 格式、编译、测试和严格检查通过。
- 直接依赖模型层的核心模块能够编译。
- 覆盖流式嵌套 JSON、合法与异常调用并存、全部调用异常以及单次请求用量场景。
- 改动仅包含 OpenAI Responses、Chat Completions 工具调用容错及对应规划和需求记录。
