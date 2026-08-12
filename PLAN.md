# 天工项目规划（PLAN）

## 总体目标

构建可扩展的个人 AI 中枢，保持桌面端、命令行和服务端共享统一的 Agent 与模型能力。

## 当前里程碑

### 0.14.x：模型协议适配完善

- 恢复并完善 OpenAI Responses API 支持。
- 保留 OpenAI Chat Completions 作为第三方兼容端点的默认协议。
- 统一文本、思考摘要、工具调用、用量和结束原因的上层行为。
- 完善 OpenAI 工具调用容错，单个异常调用不得阻断同批合法调用或结束对话。

### Fs 0.1.1：Windows 文件处理稳定性

- 修复 Windows 附件路径规范化差异导致的已发送附件误清理。
- 兼容 CRLF 文本的多行替换和统一补丁，同时保持原换行风格。
- 保持 Linux 和 macOS 的 LF 文件行为不变，由三平台流水线分别运行文件回归、构建并验证制品。
- 使用多种真实文件和 Windows 路径形式完成回归验证并独立发布插件。

## 优先级与架构边界

1. Responses 文本、流式响应和工具调用稳定可用。
2. 工具调用逐项校验和隔离，流式参数按完整 JSON 判断，用量保持单次请求语义。
3. 协议细节只存在于 `tiangong-llm` Provider 适配层。
4. `openai` 对应 Responses；`openai_chatcompletions` 和旧别名 `openai_compatible` 对应 Chat Completions。
5. 不影响 Chat Completions、Anthropic 和 DeepSeek。
6. 附件清理只删除媒体归档目录中确定未被输入、租约或消息引用的文件。
7. Fs 在文本匹配和补丁应用边界适配 LF/CRLF，输出保持目标文件原有换行风格。
8. Windows 路径展示转换只在 Windows 生效，Linux 和 macOS 保持原生路径语义。

## 关键时间节点

- 2026-08-12：基于 0.14.1 主线完成 Responses 适配和验证。
- 2026-08-12：完成 Fs 0.1.1 Windows 文件处理修复、验证和独立发布。
