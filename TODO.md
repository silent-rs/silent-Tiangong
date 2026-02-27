# TODO - 天工 RFC 0002（CLI Agent）任务清单

> 最后更新：2026-02-27
> 主线 RFC：`docs/rfc/0002-cli-agent-roadmap.md`
> 暂停 RFC：`docs/rfc/0001-tiangong-desktop-agent-roadmap.md`
> 参考：`PLAN.md`

---

## A. 文档对齐（先完成）

- [x] RFC 0002 切换为 CLI 完整 AI Agent 主线
- [x] `PLAN.md` 去除旧验证主线描述，改为“CLI Agent 主线”
- [x] `docs/requirements.md` 对齐 CLI Agent 主线目标
- [x] `README.md` CLI 用法与主线能力对齐

---

## B. M1 最小闭环（可执行任务）

### B0 架构优化（CLI 与 Core 解耦）

- [x] 新增 `src/cli/` 目录，承载 CLI 的 TUI 代码拆分（渲染、输入、事件循环）
- [x] `src/cli/` 仅放界面与交互适配，不放智能体业务逻辑
- [x] TUI 组件拆分：状态、对话框、输入框、历史对话、模型选择独立组件化
- [x] 智能体实现统一沉淀在 `src/core/`（Planner/Runtime/Tool/Session 等）
- [x] 提供 Core 层复用接口，确保 CLI 与 UI 共用同一套智能体执行链路
- [x] 清理 CLI 内部与 Core 重复实现，避免两套逻辑分叉

### B1 命令入口

- [x] `tiangong` 默认保持进入 UI 模式，不改现有默认入口行为
- [x] CLI 入口固定为 `tiangong cli`
- [x] 帮助信息覆盖 `tiangong cli` 模式示例
- [x] `main` 命令参数解析改为使用 `clap`
- [x] CLI 退出语义统一：仅 `Ctrl+C` 与 `/exit` 退出，`Esc` 仅用于清空输入

### B2 计划能力

- [x] 定义 Plan 结构：目标、`plan` 事项、事项执行步骤、风险（不包含验证命令）
- [x] 每轮任务先产出计划再执行
- [x] 计划可在执行中修正并记录变更原因
- [x] 支持 `/planing` 弹窗查看 `plan` 列表（含每个 plan 的执行步骤），并在弹窗内删除/调序 pending plan
- [x] 内置 planing 智能体生成多 `plan` 事项及执行步骤（失败自动回退最小计划并记录原因）

### B3 工具能力

- [x] 统一工具调用记录结构（名称、参数、耗时、退出码、摘要）
- [x] 具备读能力：目录浏览、文件读取、代码检索
- [x] 读能力子项：目录树查询（`tree_dir`，支持 `max_depth` 深度限制）
- [x] 具备写能力：补丁应用、定点替换、文件创建
- [x] 写能力子项：文件创建/覆盖（`write_file`）
- [x] 写能力子项：定点替换（`replace_in_file`）
- [x] 写能力子项：补丁应用（`apply_patch`）
- [x] 基础文件读写改走 `function call` 主链路
- [x] 命令执行默认强制超时
- [x] 统一命令工具为 `run_command`，并支持 `bash -lc` 受控执行（`function call`）

### B4 输出能力

- [x] 输出改动文件清单
- [x] 输出关键差异摘要
- [x] 输出本轮执行结论（完成/未完成/风险）

### B5 Skills 与 MCP 基础接入

- [x] 定义 Skills 加载与匹配机制（按任务意图选择可用 skill）
- [x] 支持 Skills 本地目录扫描与元信息索引（名称、描述、入口）
- [x] 定义 MCP 客户端抽象层（连接、资源发现、资源读取、错误处理）
- [x] 打通 Agent 执行链路中的 MCP 资源读取能力（作为上下文输入）
- [x] 统一 Skills/MCP 的执行记录与失败回传格式

---

## C. M2 工程可用（可稳定交付）

### C1 验证链路

- [x] 自动推荐验证命令（Rust 优先 `cargo check` / `cargo clippy`）
- [x] 执行验证命令并汇总结果
- [x] 验证失败时给出可操作错误摘要
- [x] 验证链路与 `plan` 结构解耦（验证信息不进入 plan 事项）

### C2 会话与状态

- [x] 任务状态机统一：`planning` / `executing` / `completed` / `failed`
- [x] 支持任务中断与恢复（最小可用）
- [x] 会话持久化写入任务级元数据
- [x] 会话索引改为扫描 `.tiangong/sessions/` 目录，不在 `app.json` 冗余 `session_ids`
- [x] `active_session_id` 对应会话不存在时自动回退至第一个可用会话
- [x] `.tiangong` 存储迁移到用户目录（Unix: `~/.tiangong`，Windows: `%USERPROFILE%\\.tiangong`），MVP 阶段不兼容旧项目目录存储（运行时不自动迁移）
- [x] 切换会话时若存在未完成 plan 事项，自动继续执行
- [x] 应用启动时若当前会话存在未完成 plan 事项，自动继续执行

### C3 非交互模式

- [ ] 支持单命令批处理模式（执行后退出）
- [ ] 批处理输出结构化摘要（便于脚本/CI 消费）

### C4 配置能力（Skills / MCP / Agent）

- [x] 设计统一配置结构（建议 `.tiangong/app.json` 扩展）支持 Skills/MCP/Agent 配置
- [x] 支持 Skills 配置项（启用列表、加载目录、黑白名单）
- [x] 支持 MCP 配置项（server 列表、连接参数、超时、重试）
- [x] 支持 CLI 配置命令或设置入口（查看/更新/校验配置）
- [x] 配置变更支持即时生效与本地持久化恢复

---

## D. M3 质量与安全

### D1 质量增强

- [ ] 执行失败重试策略（可配置）
- [ ] 计划回退与修正策略
- [ ] 长任务阶段反馈与可观测性增强
- [x] CLI 主工作区改为对话+计划分栏（3:1），右侧常驻展示 plan 与 step
- [x] planning/execution/final response 的 LLM 输出统一入对话栏展示

### D2 安全与审计

- [x] 强化工作区边界检查
- [ ] 高风险命令拦截/确认策略
- [ ] 审计日志落盘（命令、改动、时间、结果）
