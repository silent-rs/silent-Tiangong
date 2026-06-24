# 0.10.1 当前异常修复进度

> 任务索引：`README.md`
>
> 背景：0.10.0 已发布，当前进入发布后异常修复阶段。
>
> 最近更新：2026-06-24

## 当前状态

- 阶段：0.10.1 发布后异常修复进行中。
- 当前建议任务：07 - 只读工具并行执行设计。
- 当前阻塞：无。
- 目标版本建议：0.10.1。

## 进度总览

| 序号 | Issue | 任务 | 优先级 | 状态 | 开发分支 | 提交 | 验证 |
|---|---|---|---:|---|---|---|---|
| 01 | #163 | 0.10.0 发布后文档边界对齐 | P0 | 已完成 | `fix/exception-fixes-0.10.1` | `b79825c0` | `rg` 文档一致性检查、`git diff --check` |
| 02 | #166 | Workspace Index 写入器失败修复 | P0 | 已完成 | `fix/exception-fixes-0.10.1` | `8d391a42` | `cargo fmt -- --check`、`cargo check -p tiangong-core --lib`、`cargo check --workspace`、`cargo test -p tiangong-core index::workspace_index::tests::workspace_index_does_not_hold_writer_between_operations -- --nocapture`、`git diff --check` |
| 03 | #168 | 工具空参数/解析失败恢复增强 | P1 | 已完成 | `fix/exception-fixes-0.10.1` | `f1e5ea1d` | `cargo fmt -- --check`、`cargo check --workspace`、`cargo test -p tiangong-llm tool_arguments_become_parse_error -- --nocapture`、`git diff --check` |
| 04 | #167 | ReAct 主循环阶段化重构设计 | P1 | 已完成 | `fix/exception-fixes-0.10.1` | `61091f26` | 手动审查设计文档、`git diff --check` |
| 05 | #170 | 自动上下文压缩闭环核查 | P2 | 已完成 | `fix/exception-fixes-0.10.1` | `7f909847` | `cargo fmt -- --check`、`cargo check --workspace`、`cargo test -p tiangong-core observed_total_tokens -- --nocapture`、`git diff --check` |
| 06 | #165 | 工具失败恢复结构化 | P2 | 已完成 | `fix/exception-fixes-0.10.1` | 待提交 | `cargo fmt -- --check`、`cargo check --workspace`、`cargo test -p tiangong-core tool_failure -- --nocapture`、`cargo test -p tiangong-core failure_distinguishes -- --nocapture`、`git diff --check` |
| 07 | #169 | 只读工具并行执行设计 | P2 | 未开始 | 待定 | 待定 | 待定 |
| 08 | #164 | 桌面端 MCP HTTP/SSE 注册异常修复 | P0 | 已完成 | `fix/exception-fixes-0.10.1` | `bdbfd9fa` | `cargo fmt -- --check`、`cargo check --workspace`、`yarn --cwd frontend build`、`git diff --check` |

## 依赖总览

| 任务 | Issue | 前置任务 | 主要后续任务 | 可并行任务 |
|---|---|---|---|---|
| 01 | #163 | 无 | #166、#168、#167、#170、#165、#169、#164 | 无 |
| 08 | #164 | #163 | 无 | #166、#168 |
| 02 | #166 | #163 | 依赖代码检索的后续修复 | #164、#168 |
| 03 | #168 | #163 | #167、#165 | #164、#166 |
| 04 | #167 | #163、#168 | #165、#169 | #170 |
| 05 | #170 | #163 | 无 | #167 |
| 06 | #165 | #168、#167 | #169 | #170 |
| 07 | #169 | #167、#165 | 无 | 无 |

## 更新规则

- 每开始一个任务，将状态改为 `进行中`，记录开发分支。
- 每完成一个任务，将状态改为 `已完成`，记录提交和验证结果。
- 如果任务发现范围变化，先更新对应 spec，再更新本进度文件。
- 如果任务无法继续，将状态改为 `阻塞`，并在“当前阻塞”中写明原因和解除条件。
- 状态只使用：`未开始`、`进行中`、`已完成`、`阻塞`。

## 验证记录

- 2026-06-24：完成 01 文档边界对齐；检查 `PLAN.md`、`TODO.md`、`README.md`、`PROGRESS.md` 的当前主线一致性，未发现旧发布准备阶段的主线残留；`git diff --check` 通过。
- 2026-06-24：完成 08 桌面端 MCP HTTP/SSE 注册异常修复；Desktop 设置页新增 MCP 连接方式选择，stdio 继续使用 command/args/env/cwd，HTTP/SSE 端点使用 endpoint/auth/header；Tauri 命令透传 transport/endpoint/header/cwd，MCP 列表返回并展示 transport/endpoint。`cargo fmt -- --check`、`cargo check --workspace`、`yarn --cwd frontend build`、`git diff --check` 通过。尝试用 CLI 临时 HOME 做端到端注册验证时卡在应用启动链路，已终止；尝试运行聚焦 cargo test 时首次编译依赖超过 120 秒限时，未跑到测试本体。
- 2026-06-24：完成 02 Workspace Index 写入器失败修复；Workspace 索引不再长期持有 Tantivy writer，扫描、增量写入和删除时短暂创建 writer 并提交释放；打开失败时错误包含 workspace、索引目录和阶段，损坏索引可自动重建目录。新增单测覆盖同一 workspace 被两个索引实例连续扫描的场景。`cargo fmt -- --check`、`cargo check -p tiangong-core --lib`、`cargo check --workspace`、`cargo test -p tiangong-core index::workspace_index::tests::workspace_index_does_not_hold_writer_between_operations -- --nocapture`、`git diff --check` 通过。
- 2026-06-24：完成 03 工具空参数/解析失败恢复增强；OpenAI Chat Completions 与 DeepSeek 非流式工具参数解析失败不再静默转为空对象，改为保留结构化解析错误并进入 ReAct 失败恢复链路；空参数、非法 JSON 和重复失败提示均要求重新生成完整 JSON，避免把 `__parse_error` 当作真实参数。新增单测覆盖 OpenAI/DeepSeek 的空参数与非法 JSON 场景。`cargo fmt -- --check`、`cargo check --workspace`、`cargo test -p tiangong-llm tool_arguments_become_parse_error -- --nocapture`、`git diff --check` 通过。
- 2026-06-24：完成 04 ReAct 主循环阶段化重构设计；已梳理 `execute_turn` 当前职责，定义 `prepare_turn`、`prepare_round`、`drain_commands`、`run_model_stream`、`execute_tool_calls`、`handle_failure_recovery`、`finalize_turn` 等阶段的输入、输出、副作用和中断处理，明确行为不变清单，并拆分 04-A 到 04-F 的后续重构任务边界。仅文档变更，按 spec 手动审查设计文档，`git diff --check` 通过。
- 2026-06-24：完成 05 自动上下文压缩闭环核查；对照 `docs/requirements.md` 核查自动压缩触发、GUI 反馈、摘要注入和 token 统计链路。修复自动压缩只看 `prompt_tokens` 的缺口，改为按本次请求总 token 判断，Provider 未返回 total 时回退到 prompt+completion。记录后续缺口：自动压缩失败的滑动窗口降级、`compress_loop_messages` 接入 ReAct 主循环、工具结果预算与截断策略。`cargo fmt -- --check`、`cargo check --workspace`、`cargo test -p tiangong-core observed_total_tokens -- --nocapture`、`git diff --check` 通过。
- 2026-06-24：完成 06 工具失败恢复结构化；新增工具失败结构字段和分类，参数错误、权限拒绝、用户拒绝、命令失败、超时、环境缺失、网络失败、工具内部异常会写成模型可读的 `[tool_failure]` 结构化 tool result；重复失败会标记 `same_failure_count` 并要求不要重复同一调用。`StreamEvent::ToolResult.output` 保持原有短文本，不要求前端新增面板。`cargo fmt -- --check`、`cargo check --workspace`、`cargo test -p tiangong-core tool_failure -- --nocapture`、`cargo test -p tiangong-core failure_distinguishes -- --nocapture`、`git diff --check` 通过。
