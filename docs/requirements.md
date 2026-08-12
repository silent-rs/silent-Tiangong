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

## 编辑历史消息重发性能优化

### 必须满足

- 编辑历史用户消息并重发时复用既有 Core，不再每次销毁重建（与普通发送路径一致）。
- 复用 Core 不得引入对话覆盖竞态：编辑重发已校验无活跃 turn，Core 不主动写盘。
- `on_session_ready` 每个 Core 实例只调用一次（首次 turn），不随每轮 turn 或编辑重发重复触发；编辑重发复用 Core 时通过 `on_turn_started` 处理每轮增量。
- 终端插件的每轮终端状态注入走 `on_turn_started`，`on_session_ready` 只保留首次基线设置。
- deliver 失败时磁盘 session 能回滚到编辑前状态，且不销毁被复用的 Core。
- 会话存在性校验使用单文件存在性判定，不再遍历解析全部会话文件。
- 前端编辑重发前不再阻塞等待主输入框草稿同步（改为后台落盘），且不依赖该同步结果。
- 前端发送确认时不对已脱离原消息的内容块重复深拷贝。

### 完成标准

- `tiangong-app` 编译和 clippy 检查通过。
- 前端 `tsc -b` 和 `vite build` 通过。
- 编辑重发后流式输出、连续多轮编辑、带附件编辑重发、deliver 失败回滚均正常。
- 同一 Core 连续两轮：`on_session_ready` 仅调用 1 次，`on_turn_started`/`on_turn_finished` 各 2 次；编辑重发不增加 `on_session_ready` 次数。
- 终端插件在连续轮次和编辑重发后均能注入最新终端状态，且不重复注入已推送输出。
