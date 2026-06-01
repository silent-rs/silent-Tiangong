# 天工项目规划（PLAN）

## 愿景

构建全功能可扩展的 GUI + CLI + Server 个人智能终端平台，实现"可对话、可规划、可执行、可扩展、可治理、可远程"的 Agent 能力闭环。通过 IM Adapter 接入消息通道，通过自动化触发层实现系统级主动调度，打造个人 AI 中枢。

## 版本规划

| 版本 | 里程碑 | 状态 |
|------|--------|------|
| 0.1.0 | CLI Agent 基线 + Skill 管理 | ✅ |
| 0.2.0 | Server + Gateway + 多媒体 + Memory | ✅ |
| 0.3.0 | 多智能体协作 + 发布分发 + 飞书互联 | ✅ |
| **0.4.0** | **自动化触发层（cron/webhook/polling）** | **✅** |
| **0.5.0** | **模型配置重构与多模态会话能力收敛** | **发布准备中** |

## 当前执行策略（2026-06-01）

- Phase 1~20 已完成。
- 当前主目标：基于最新主分支发布 0.5.0。
- 0.5.0 聚焦模型配置重构、多模态会话模型能力收敛与发布流水线版本同步。

## 已完成里程碑

| Phase | 名称 | 关键交付 |
|-------|------|----------|
| 1 | CLI Agent 基线 | 单 Agent 对话、MCP 接入 |
| 2 | Skill 管理 MVP | 安装/启停/卸载、动态 Step 执行 |
| 3 | Workspace 拆分 | 多 crate 分离（core / cli / gui） |
| 4 | Server 模式 | HTTP + WebSocket API、Token 认证、守护进程 |
| 5 | Gateway 与事件总线 | EventBus、统一消息路由 |
| 6 | IM Adapter 框架 | 外部 Bot 统一入口、飞书 Bot Adapter |
| 7 | 多媒体能力 | 图片/视频生成、语音识别/合成 |
| 8 | 生产化与完善 | 日志监控、配置热重载、TLS |
| 9 | 模型配置重构 | Provider + Model + Routing 三层架构、shadcn/ui |
| 10 | 友好交互改造 | GUI 简化、流式展示 |
| 11 | 运行时基础设施 | 统一任务模型、后台任务回流、远程角色模型 |
| 12 | 事件驱动运行时 | Event-loop 模型替代 Turn-based |
| 13 | CoreConfig 注入 | 配置外部注入，支持 CLI/GUI/Server |
| 15 | LLM 协议抽象 | tiangong-llm + tiangong-anthropic、Anthropic 适配 |
| 16 | 架构收口 | 统一入口、成本可见性、安全审计 |
| 17 | 多媒体语义收敛 | 结构化图片/视频结果、Connector 视频发送 |
| 18 | Memory 系统 | tiangong-memory crate、Micro/Meso 记忆链路 |
| 19 | 多智能体协作 | 动态组队、消息通讯、文件编辑锁、前端交互 |

## Phase 20：自动化触发层（已完成）

> Issue：#38

内嵌于 Server 的系统级主动触发能力，与 IM Adapter 模式正交。

**设计原则**：
- 调度器常驻 Server 进程，不做成 tool call
- Job 持久化（SQLite），启动时恢复
- 触发 → RuntimeEvent → 现有执行链路
- 结果可投递到 IM 通道

| 子阶段 | 内容 |
|--------|------|
| 20-A | 任务模型与存储 — Job/JobRun/JobDelivery 模型、SQLite store、CRUD API |
| 20-B | Cron 调度器 — 表达式解析、常驻执行器、启动恢复、手动触发 |
| 20-C | Webhook 触发器 — 端点注册、签名验证、触发接入 |
| 20-D | Polling 触发器 — HTTP 轮询、条件去重、触发接入 |
| 20-E | 结果投递与通知 — IM 通道投递、失败重试、Run history API |
| 20-F | 前端管理界面 — Job 列表/创建/启停、执行历史、手动触发 |

## 参考文档

- 架构基准：`docs/desktop-agent-technical-architecture.md`
- 需求基线：`docs/requirements.md`
- Server API：`docs/server-api.md`
- RFC 0011：`docs/rfc/0011-multi-agent-collaboration.md`
