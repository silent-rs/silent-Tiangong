# 天工需求整理

## 文档目的
用于对齐天工当前阶段（RFC 0002）的开发边界，作为 `PLAN.md`、`TODO.md` 与实现代码的一致性基线。

## 当前范围（RFC 0002：CLI Agent 主线）

### Must
- `tiangong` 默认保持桌面 UI 入口，不改变现有默认行为。
- 提供 CLI Agent 入口 `tiangong cli`。
- `main` 命令参数解析需使用 `clap` 实现，统一子命令与帮助输出行为。
- CLI 交互界面使用 `ratatui`，支持连续多轮任务执行与会话恢复。
- 主界面交互规则：输入以 `/` 开头且命令提示可见时，`↑/↓` 用于命令项选择；普通输入时 `Shift+↑/↓` 切换输入历史；当输入框为空且存在历史时，`↑/↓` 直接切换输入历史；其他普通输入场景下 `↑/↓` 用于输入框光标移动；鼠标滚轮用于对话区滚动。
- CLI 需提供 `/planing` 弹窗（与“切换会话”弹窗交互风格一致）查看当前 planning 列表。
- 状态栏需支持复合阶段显示（如 `planning + executing`），并展示当前执行中的 plan/step（含序号与进度）。
- CLI 主工作区采用左右分栏，左侧对话区与右侧 plan 区固定为 `3:1` 布局；右侧需常驻展示当前任务的 plan 与 step 状态。
- 同一会话内，plan 需保持完整历史：新增输入只追加新 plan，不清理已完成 plan，不自动调整历史顺序；未完成（pending）plan 允许用户手动调序与删改。
- planning 的删除与调序仅在 `/planing` 弹窗内完成，且只允许操作未开始（pending）plan 事项。
- planning 列表中的已完成 plan 事项需以删除线样式显示。
- CLI 对话输出需支持 Markdown 轻量渲染（至少覆盖标题、粗体、列表、代码块），提升终端可读性。
- CLI 层仅承载 TUI 与交互适配代码；智能体能力统一沉淀在 `src/core/agents/`，运行编排能力在 `src/core/runtime.rs`，两者职责清晰分层并供 UI 与 CLI 复用。
- 建立最小执行链路：输入 -> 规划 -> 执行 -> 输出。
- 规划阶段需内置 planing 智能体（Planning Agent），优先由模型生成结构化计划。
- 规划结果至少包含：目标、`plan` 事项列表、每个 `plan` 的独立执行步骤列表、风险。
- `plan` 的每个 `execution step` 必须由执行智能体逐步驱动执行；仅在步骤执行成功后才允许标记为完成。
- 当某个 `execution step` 执行失败时：该步骤标记为 `failed`，同一 `plan` 中后续未执行步骤标记为 `ignored`，并汇总该 `plan` 执行结果后继续执行下一个 `plan`。
- `plan` 的调整仅允许在规划模型阶段完成（由用户新输入触发）；执行阶段不允许基于工具结果自动扩增或改写 `plan`。
- 允许用户多次输入形成“连续输入列表”，规划模型需基于该列表进行增量规划并输出调整后的有序 `plan`。
- `plan` 仅表达待办事项，不承载验证命令或验证结论；验证链路在运行时独立执行。
- 工具执行需支持结构化记录（工具名、参数、耗时、退出码、摘要）。
- 基础文件读写改能力（`read_file` / `write_file` / `replace_in_file` / `apply_patch`）需优先通过 `function call` 驱动执行。
- 工具能力至少包含：
  - 读：`list_dir`、`tree_dir`（支持 `max_depth` 深度限制）、`read_file`、代码检索。
  - 写：`write_file`、`replace_in_file`、`apply_patch`。
  - 命令：统一使用受控 `run_command`（支持 `cmd=bash,args=["-lc","..."]`），默认强制超时。
- 工具与文件写入必须限制在工作区边界内，不允许越界访问。
- 每轮输出需包含改动文件概览、差异摘要与执行结论（完成/未完成/风险）。
- 会话数据本地持久化到用户目录（Unix: `~/.tiangong/sessions/`，Windows: `%USERPROFILE%\\.tiangong\\sessions\\`），应用配置持久化到对应的 `app.json`。
- CLI 需提供 `/sessions` 命令用于“切换会话”。
- 当用户切换会话时，如果该会话存在未完成（pending）的当前任务 plan 事项，系统需自动继续执行该 plan。
- 应用启动后，若当前激活会话存在未完成（pending）的当前任务 plan 事项，系统需自动继续执行该 plan。
- MVP 阶段运行时不自动迁移项目目录内旧 `.tiangong/` 数据。
- 会话文件名必须使用 `scru128`（如 `<scru128>.json`）。
- 统一配置结构需覆盖 Model/Skills/MCP/Agent，并支持本地恢复。
- CLI 必须提供配置入口，至少支持配置查看、更新和校验，并在更新后即时生效。
- 模型适配需支持 BigModel `thinking` 参数透传，并正确解析/持久化 `reasoning_content`（与正文区分）。
- 所有 LLM 调用输出（planning / execution / final response）均需进入对话栏可见范围，不允许仅在内部状态保存。
- `async-openai` 允许暂时使用本地 fork 版本，以承载上述兼容字段能力。

### Should
- 当 planing 智能体不可用或返回非法结构时，自动回退到最小计划策略并记录原因。
- 自动推荐并执行验证命令（Rust 优先 `cargo check` / `cargo clippy`），失败返回可操作摘要。
- 验证能力与 `plan` 解耦：验证失败不应作为 `plan` 事项描述的一部分。
- 支持 Skills 本地扫描、索引与按任务意图匹配。
- 定义 MCP 客户端抽象（连接、资源发现、资源读取、错误处理）。
- 将 MCP 资源读取结果接入执行链路，作为模型上下文输入。
- 统一 Skills/MCP 的执行记录与失败回传格式，便于审计与排障。

### 非目标（当前阶段不做）
- 多模型供应商并行路由。
- 分布式执行与远程 Worker。
- 全量插件生态与复杂插件市场能力。

## 参考
- `README.md`
- `PLAN.md`
- `TODO.md`
- `docs/rfc/0002-cli-agent-roadmap.md`
