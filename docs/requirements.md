# 天工需求整理

## 文档目的
用于对齐天工当前阶段（RFC 0003）的开发边界，作为 `PLAN.md`、`TODO.md` 与实现代码的一致性基线。

## 已有基线能力（承接 RFC 0002）

以下能力视为当前稳定基线，不在本轮重做：

- `tiangong` 默认保持桌面 UI 入口，CLI 入口为 `tiangong cli`。
- CLI 具备任务闭环：输入 -> planning -> executing -> final response。
- MCP 已支持本地 `stdio` 与远程 HTTP（JSON-RPC over HTTP）并存。
- Agent 配置已支持查看、更新、校验与即时生效。
- Skills 已支持本地扫描、索引与按任务意图匹配。

## 当前范围（RFC 0003：Skill 能力管理）

### Must

- Skill 管理方式必须与 MCP 管理一致：支持查看、筛选、启停、增删、校验、持久化。
- CLI 必须提供 `/skill` 管理入口，交互风格对齐 `/mcp` 管理弹窗。
- MCP 管理必须支持 JSON 导入：`tiangong mcp add --json '<json>'`。
- JSON 导入必须支持两类输入：单个 MCP server 对象、`mcpServers` 映射对象。
- JSON 导入必须采用确定性映射规则完成字段转换，不依赖大模型推断。
- JSON 导入必须在落盘前执行 MCP 配置校验（含 transport/endpoint/args/env/cwd 约束）并返回可定位错误。
- JSON 导入遇到同名 server 默认拒绝写入并返回冲突提示（除非显式指定覆盖策略）。
- 启动阶段必须对已启用 MCP server 进行能力预热，至少拉取 `tools/list` 与 `resources/list` 并建立内存索引。
- MCP 能力数据必须支持持久化缓存，至少包含每个 server 的 `tools` 列表，并落盘到 `~/.tiangong`。
- MCP `tools` 缓存必须保存完整元数据（至少 `name`、`description`、`inputSchema`、参数必填信息与类型摘要），不能仅保存工具名列表。
- `mcp-tools-cache.json` 不再维护 `keywords` 字段；缓存与对话侧仅基于启用 server 的 tools 元数据。
- 启动后必须按固定周期刷新已启用 MCP server 的能力缓存（含 `tools/list`），刷新失败需保留最近一次可用缓存并记录错误。
- 对话阶段必须将“已启用 MCP server 的 tools 元数据”直接注入 system prompt，不再依赖“先匹配 server 再筛选 tools”的链路。
- Skill 必须支持本地安装、启用、停用、卸载、列表、详情展示。
- Skill 依赖 MCP 时，必须通过受控安装器生成托管 MCP server 配置，不允许 Skill 注入任意 `command/args`。
- Skill 依赖 MCP 的命名与映射必须可追踪：`skill::<skill_id>::<mcp_id>`。
- Skill 卸载时必须仅移除其托管 MCP 配置；共享依赖需基于引用计数处理。
- Skill/MCP 管理操作必须统一结构化执行记录与失败回传格式，便于审计与排障。
- 配置与锁文件必须统一写入用户目录 `~/.tiangong`，不得引入并行存储根。
- MCP 与 Skill 必须拆分为独立配置文件，不得继续混存于 `app.json`：
  - `mcp` 配置写入 `~/.tiangong/mcp.json`
  - `skills` 配置写入 `~/.tiangong/skills.json`
- `app.json` 仅保留会话/模型/UI 等应用状态，不再承载 `mcp` 与 `skills` 配置。
- 必须新增并维护 `skills-lock.json`、`mcp-lock.json`，并与 `mcp.json`、`skills.json` 保持一致。
- 安装流程必须具备事务与回滚：任一步骤失败后回滚增量变更并输出错误分类。
- 安装前必须输出权限摘要（`fs_read/fs_write/cmd_exec/net`）与依赖摘要，并要求用户确认。
- 文件/命令/网络权限必须默认最小化：
  - 文件访问需 canonicalize，且仅允许当前工作目录、`~/.tiangong` 与临时目录（`/tmp`）边界。
  - 命令执行仅允许白名单命令，禁止 `bash -c` 等复合注入模式。
  - 网络默认 deny，首期仅允许显式 MCP HTTP endpoint。

### Should

- 支持 Git 源安装 Skill（在本地源稳定后交付）。
- 支持非交互命令：`tiangong skill list/install/remove/enable/disable/validate`。
- MCP JSON 导入应支持文件输入：`tiangong mcp add --json-file <path>`，避免命令行转义复杂度。
- MCP 能力缓存缺失时应支持启动后异步兜底刷新并回填，不阻塞主对话流程。
- 支持 Skill 包 `skill.toml` 与 `SKILL.md` 双格式兼容（缺失 `skill.toml` 时降级解析）。
- 支持外部 Skill 快速转换为天工 Skill（`--convert` 自动补齐 `SKILL.md/skill.toml`）。
- `--convert` 必须优先使用天工内置智能体 + 大模型进行辅助转换；模型不可用或生成失败时回退固定规则转换。
- `--convert` 转换结束后必须自动清理 `~/.tiangong/skills/imported` 中对应的转换中间目录，避免残留。
- 命中 Skill 后，planning/execution 阶段必须注入 Skill 上下文（含 `SKILL.md` 关键说明），确保执行阶段可实际消费 Skill 指令。
- Skill 依赖命令执行时，受控命令白名单需覆盖常见运行时（如 `node/npx/npm/yarn/pnpm/ts-node`），并在 Skill 目录执行时支持从 `.env.local/.env` 加载环境变量。
- 程序运行时需汇总已启用 Skill 与 MCP 的环境配置（Skill 目录 `.env.local/.env`、MCP server `env`），统一注入受控命令执行环境，且 `cwd` 下 `.env.local/.env` 仍可按局部优先覆盖。
- 执行阶段应采用动态 step 推进：初始计划仅保留可执行事项（plan item），每个 step 在执行后都需判断是否继续、补充或终止后续 step，避免预置 step 缺失导致误完成。
- 会话记录应以“实际执行的 step 轨迹”为准，不应仅依赖规划阶段一次性生成的静态 step 列表。
- 会话中的 step 记录应标注来源（`planned/dynamic`），便于追踪动态补充链路与排障。
- 支持 Skill 初始化命令（`/skill init`）快速生成天工兼容脚手架（`SKILL.md`、`skill.toml`）。

### 非目标（当前阶段不做）

- 商业化支付与计费体系。
- 评分/推荐/排行榜。
- GUI Web Market。
- 去中心化分发与 P2P 镜像网络。
- 复杂组织级权限与审核系统。

## 参考

- `README.md`
- `PLAN.md`
- `TODO.md`
- `docs/rfc/0002-cli-agent-roadmap.md`
- `docs/rfc/0003-skill-market.md`
