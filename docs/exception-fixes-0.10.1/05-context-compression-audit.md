# 05 - 自动上下文压缩闭环核查

## 目标

核查并补齐 ReAct 执行过程中自动上下文压缩的闭环，确保长对话、工具结果堆积和 token 阈值场景符合需求文档要求。

## 范围

- `docs/requirements.md` 中上下文管理相关要求
- `crates/tiangong-core/src/react/context.rs`
- `crates/tiangong-core/src/react/engine.rs`
- `crates/tiangong-core/src/core/` 中上下文压缩入口
- GUI 压缩开始/完成反馈相关事件，如已有实现

## 依赖

- 前置任务：01
- 后续任务：无
- 可并行任务：04
- 阻塞说明：01 未完成前，不应确认本任务是否属于 0.10.1 修复范围。

## 任务

- 对照 `docs/requirements.md` 梳理上下文压缩必须满足的条目。
- 核查 token usage 达到阈值时是否自动触发压缩。
- 核查 ReAct 单轮内工具结果大量堆积时是否会摘要或压缩。
- 核查压缩开始、完成、失败是否有用户可见反馈。
- 核查压缩失败是否回退到滑动窗口或其他降级策略。
- 核查压缩后的 system prompt、自定义 Prompt 和摘要注入位置是否符合要求。
- 如发现缺口，拆分后续修复任务；小缺口可在本任务内修复。

## 核查结论

| 要求 | 当前结论 | 证据/处理 |
|---|---|---|
| 达到上下文限制 95% 自动触发压缩 | 已满足，且本任务修正为按本次请求总 token 判断 | `react/context.rs::maybe_update_context_summary` 使用 `TokenUsage` 的 `total_tokens`，缺失时回退到 `prompt_tokens + completion_tokens` |
| 收到 LLM 输出后结合输入与输出 token 判断 | 已修复 | 原实现只传 `response.usage.prompt_tokens`；本任务改为传完整 `response.usage` |
| GUI 展示当前上下文 tokens、压缩阈值和累计 tokens | 已满足 | `StreamEvent::TokenUsage` 写入 session token 字段，`MessageInput` 展示进度条和总 tokens |
| 会话相关 LLM token 进入统计 | 基本满足 | 主对话、工具相关 LLM 用量、记忆召回、上下文摘要用量均通过 `emit_token_usage` 或 session 统计链路进入累计值；仍需在后续大改时保持该链路 |
| 优先 LLM 摘要，失败时回退滑动窗口 | 部分满足 | 手动压缩失败会给用户错误；自动压缩失败只记录 warn 并继续原上下文，尚未实现滑动窗口截断降级 |
| ReAct 单轮内工具结果大量堆积时压缩 loop messages | 未闭环 | `context/compressor.rs::compress_loop_messages` 已存在，但当前 `execute_turn` 没有调用 |
| 无状态 Chat API 多轮上下文组织 | 已满足 | `Session::context()` 返回 system prompt 加 `summary_up_to` 后的消息链 |
| 高波动内容不进入稳定 system prompt | 基本满足 | 工具反馈、LLM 输出记录、Memory recall 通过 tool/message 链路进入会话；摘要才进入 system prompt |
| 压缩摘要注入 system prompt | 已满足 | `maybe_update_context_summary` 压缩成功后调用 `rebuild_system_prompt`，`Session::context()` 把 system prompt 置前 |
| 用户自定义 Prompt 压缩后仍保留 | 已满足 | 重建 system prompt 走 `SystemPromptConfig::from_configs`，保持配置段和插件段 |
| GUI 压缩开始/完成可见反馈 | 已满足 | `ContextCompressing` 更新运行摘要为“正在压缩早期上下文...”，`ContextCompressed` 更新状态并写入系统消息 |

## 本任务内修复

- 自动压缩触发依据从单独 `prompt_tokens` 调整为本次请求总 tokens。
- 当 Provider 未返回 `total_tokens` 时，使用 `prompt_tokens + completion_tokens` 兜底。
- 上下文超限强制压缩路径继续按 context limit 构造触发用量，不改变现有重试语义。
- 增加纯函数测试覆盖 total 优先与兜底求和。

## 后续缺口

| 缺口 | 建议后续任务 | 原因 |
|---|---|---|
| 自动压缩失败没有滑动窗口截断降级 | 05-A 自动压缩失败降级策略 | 需要决定截断边界、用户可见提示和 system prompt 重建方式，不应夹在本次小修内 |
| `compress_loop_messages` 未接入 ReAct 主循环 | 05-B ReAct loop message 压缩接入 | 需要结合 04 的阶段化设计，避免在当前大函数里新增复杂控制流 |
| 大量工具结果写入后才压缩，工具执行中缺少输出预算门槛 | 05-C 工具结果预算与截断策略 | 需要统一工具结果大小、模型可见文本和 UI 完整输出之间的关系 |

## 不做

- 不重写上下文压缩算法。
- 不改变 Memory recall 语义。
- 不做 UI 大改版。
- 不修改模型协议适配。

## 验收

- 形成上下文压缩核查结论。
- 明确列出已满足项、缺口项和后续任务。
- 如果本任务内修复缺口，必须有验证记录。
- 长上下文接近阈值时不会无提示地继续膨胀到失败。

## 验证

- `cargo fmt -- --check`，如果修改代码。
- `cargo check --workspace`，如果修改代码。
- 手动或测试模拟接近上下文阈值的会话。
- 手动或测试模拟大量工具结果堆积。
