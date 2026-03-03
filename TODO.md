# TODO - 天工 RFC 0003（Skill 管理）任务清单

> 最后更新：2026-03-03
> 当前主线 RFC：`docs/rfc/0003-skill-market.md`
> 基线 RFC：`docs/rfc/0002-cli-agent-roadmap.md`
> 参考：`PLAN.md`、`docs/requirements.md`

---

## A. 文档与范围对齐（先完成）

- [x] `docs/rfc/0003-skill-market.md` 重构为可开发的 v0.2（聚焦 MVP）
- [x] `docs/requirements.md` 对齐“Skill 管理与 MCP 一致”
- [x] `PLAN.md` 切换当前增量主线至 RFC 0003
- [x] `TODO.md` 拆分 RFC 0003 首批可实施任务

---

## B. M1 数据结构与配置层

### B1 Skill 配置模型扩展

- [ ] 在 `AgentConfig` 中新增 Skill 安装态字段（installed/source/enabled/managed_mcp）
- [ ] 定义 `skill.toml` 的最小解析结构与校验规则
- [ ] 保持 `SKILL.md` 兼容降级路径（缺 `skill.toml` 时可加载）

依赖：无

### B2 锁文件模型与持久化

- [ ] 新增 `skills-lock.json` 读写与校验
- [ ] 新增 `mcp-lock.json` 读写与引用计数逻辑
- [ ] 保证 `app.json`、`skills-lock.json`、`mcp-lock.json` 一致性更新

依赖：B1

---

## C. M1 Skill 生命周期管理

### C1 SkillManager 核心能力

- [ ] 实现 `install_skill`（本地目录）
- [ ] 实现 `remove_skill`
- [ ] 实现 `set_skill_enabled`
- [ ] 实现 `list_skill` / `describe_skill`

依赖：B1、B2

### C2 Skill -> MCP 托管映射

- [ ] 解析 `requires.mcp` 并生成托管 server 名：`skill::<skill_id>::<mcp_id>`
- [ ] 通过受控模板生成 MCP 配置（禁止 Skill 注入任意 command/args）
- [ ] 卸载 Skill 时移除托管 MCP 配置并维护依赖引用计数

依赖：C1

### C3 事务与回滚

- [ ] 安装流程分阶段提交（解析 -> 安装依赖 -> 写锁 -> 注入配置 -> 校验）
- [ ] 任一步骤失败后执行增量回滚
- [ ] 输出结构化错误分类（解析失败/依赖失败/配置失败/写入失败）

依赖：C1、C2

---

## D. M1 CLI/TUI 管理入口（对齐 MCP）

### D1 命令入口

- [ ] 新增 `/skill` 命令（支持 `/skill <query>`）
- [ ] 补充命令提示与帮助文本

依赖：C1

### D2 Skill 管理弹窗

- [ ] 新增 Skill 弹窗组件（布局与 `/mcp` 风格一致）
- [ ] 支持筛选、上下选择、详情展示、启停、删除、新增
- [ ] 状态栏反馈统一为结构化文案

依赖：D1、C1

---

## E. M1 安全与审计

### E1 权限与确认

- [ ] 安装前展示权限摘要与依赖摘要
- [ ] 增加安装确认流程（用户确认后继续）
- [ ] 命令权限白名单与高风险模式拒绝（禁 `bash -c`）

依赖：C1、C2

### E2 审计记录

- [ ] Skill/MCP 管理事件统一结构（event_id/type/status/duration/error/time）
- [ ] 安装/卸载/启停失败均可追踪到事件记录

依赖：C1、C2

---

## F. M1 验证与交付

- [ ] 完成 `cargo fmt -- --check`
- [ ] 完成 `cargo check --workspace`
- [ ] 完成 `cargo clippy --workspace --all-targets --tests --benches -- -D warnings`
- [ ] 更新 TODO 状态并准备 PR 描述

依赖：B~E 全部完成
