# 天工项目规划（PLAN）

## 愿景
构建一个基于 Hetu 的桌面智能中枢系统，实现“可对话、可规划、可执行、可扩展、可治理”的 Agent 能力闭环。

## 总体目标
- 产品目标：从桌面对话入口演进为系统级自动化执行平台。
- 架构目标：保持 UI、核心引擎、模型层、Skill/MCP 层解耦。
- 安全目标：在工具执行与扩展能力链路落实最小权限、审计与可追踪。
- 工程目标：采用小步迭代，按 Phase 分期交付可运行版本。

## 当前执行策略（2026-03-03）
- 基线主线：`docs/rfc/0002-cli-agent-roadmap.md`（已形成 CLI Agent 可用闭环）。
- 当前增量主线：`docs/rfc/0003-skill-market.md`（先做 Skill 管理能力，不做复杂市场化能力）。
- 当前目标：交付“与 MCP 一致”的 Skill 生命周期管理与依赖托管能力。

## 里程碑

### Phase 1（CLI Agent 基线，已达成）
- 单 Agent 对话能力可用。
- 最小任务执行链路可跑通（输入 -> 规划 -> 执行 -> 反馈）。
- MCP 本地/远程接入可用。

### Phase 2（Skill 管理 MVP，进行中）
- Skill 支持安装、启停、卸载、列表、详情。
- `/skill` 管理交互对齐 `/mcp`。
- Skill -> MCP 依赖映射自动化与托管。
- `app.json` / `skills-lock.json` / `mcp-lock.json` 一致性与回滚保障。

### Phase 3（Skill 来源扩展）
- Git 源 Skill 安装能力。
- 非交互 `tiangong skill ...` 子命令。
- 远程 registry 只读索引与下载。

### Phase 4（系统自动化与治理）
- 审计能力增强（落盘、检索、追踪）。
- 高风险命令拦截与确认策略完善。
- 执行稳定性提升（失败重试、恢复策略）。

### Phase 5（生态扩展）
- 可选的签名验证与审核机制。
- 企业私有源与离线镜像能力。
- 在安全前提下逐步评估市场化能力。

## 参考文档
- 项目说明：`README.md`
- RFC 0002：`docs/rfc/0002-cli-agent-roadmap.md`
- RFC 0003：`docs/rfc/0003-skill-market.md`
- 需求基线：`docs/requirements.md`
