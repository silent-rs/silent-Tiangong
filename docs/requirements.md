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
- Skill 必须支持本地安装、启用、停用、卸载、列表、详情展示。
- Skill 依赖 MCP 时，必须通过受控安装器生成托管 MCP server 配置，不允许 Skill 注入任意 `command/args`。
- Skill 依赖 MCP 的命名与映射必须可追踪：`skill::<skill_id>::<mcp_id>`。
- Skill 卸载时必须仅移除其托管 MCP 配置；共享依赖需基于引用计数处理。
- Skill/MCP 管理操作必须统一结构化执行记录与失败回传格式，便于审计与排障。
- 配置与锁文件必须统一写入用户目录 `~/.tiangong`，不得引入并行存储根。
- 必须新增并维护 `skills-lock.json`、`mcp-lock.json`，保证与 `app.json` 一致。
- 安装流程必须具备事务与回滚：任一步骤失败后回滚增量变更并输出错误分类。
- 安装前必须输出权限摘要（`fs_read/fs_write/cmd_exec/net`）与依赖摘要，并要求用户确认。
- 文件/命令/网络权限必须默认最小化：
  - 文件访问需 canonicalize 且受工作区边界约束。
  - 命令执行仅允许白名单命令，禁止 `bash -c` 等复合注入模式。
  - 网络默认 deny，首期仅允许显式 MCP HTTP endpoint。

### Should

- 支持 Git 源安装 Skill（在本地源稳定后交付）。
- 支持非交互命令：`tiangong skill list/install/remove/enable/disable/validate`。
- 支持 Skill 包 `skill.toml` 与 `SKILL.md` 双格式兼容（缺失 `skill.toml` 时降级解析）。

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
