# 天工需求整理

## 文档目的
用于对齐天工当前阶段（RFC 0002）的开发边界，作为 `PLAN.md`、`TODO.md` 与实现代码的一致性基线。

## 当前范围（RFC 0002：CLI Agent 主线）

### Must
- `tiangong` 默认保持桌面 UI 入口，不改变现有默认行为。
- 提供 CLI Agent 入口 `tiangong cli`。
- CLI 交互界面使用 `ratatui`，支持连续多轮任务执行与会话恢复。
- CLI 需提供 `/planing` 弹窗（与历史会话弹窗交互风格一致）查看当前 planning 列表。
- planning 的删除与调序仅在 `/planing` 弹窗内完成，且只允许操作未开始（pending）步骤。
- planning 列表中的已完成步骤需以删除线样式显示。
- CLI 对话输出需支持 Markdown 轻量渲染（至少覆盖标题、粗体、列表、代码块），提升终端可读性。
- CLI 层仅承载 TUI 与交互适配代码；智能体能力统一沉淀在 `src/core/`，供 UI 与 CLI 复用。
- 建立最小执行链路：输入 -> 规划 -> 执行 -> 输出。
- 规划结果至少包含：目标、步骤、风险、预期验证命令。
- 工具执行需支持结构化记录（工具名、参数、耗时、退出码、摘要）。
- 工具能力至少包含：
  - 读：`list_dir`、`read_file`、代码检索。
  - 写：`write_file`、`replace_in_file`、`apply_patch`。
  - 命令：受控 `run_command`，且默认强制超时。
- 工具与文件写入必须限制在工作区边界内，不允许越界访问。
- 每轮输出需包含改动文件概览、差异摘要与执行结论（完成/未完成/风险）。
- 会话数据本地持久化到用户目录（Unix: `~/.tiangong/sessions/`，Windows: `%USERPROFILE%\\.tiangong\\sessions\\`），应用配置持久化到对应的 `app.json`。
- MVP 阶段运行时不自动迁移项目目录内旧 `.tiangong/` 数据。
- 会话文件名必须使用 `scru128`（如 `<scru128>.json`）。
- 统一配置结构需覆盖 Model/Skills/MCP/Agent，并支持本地恢复。
- CLI 必须提供配置入口，至少支持配置查看、更新和校验，并在更新后即时生效。
- 模型适配需支持 BigModel `thinking` 参数透传，并正确解析/持久化 `reasoning_content`（与正文区分）。
- `async-openai` 允许暂时使用本地 fork 版本，以承载上述兼容字段能力。

### Should
- 计划在执行中可修正，并记录修正原因。
- 自动推荐并执行验证命令（Rust 优先 `cargo check` / `cargo clippy`），失败返回可操作摘要。
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
