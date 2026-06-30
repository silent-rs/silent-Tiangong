# 天工项目规划（PLAN）

## 愿景

构建全功能可扩展的 GUI + CLI + Server 个人智能终端平台，实现"可对话、可规划、可执行、可扩展、可治理、可远程"的 Agent 能力闭环。通过 IM Adapter 接入消息通道，通过自动化触发层实现系统级主动调度，打造个人 AI 中枢。

## 版本规划

| 版本 | 里程碑 | 状态 |
|------|--------|------|
| 0.1.0 | CLI Agent 基线 + Skill 管理 | ✅ |
| 0.2.0 | Server + Gateway + 多媒体 + Memory | ✅ |
| 0.3.0 | 多智能体协作 + 发布分发 + 飞书互联 | ✅ |
| 0.4.0 | 自动化触发层（cron/webhook/polling） | ✅ |
| 0.5.0 | 模型配置重构与 DeepSeek 支持 | ✅ |
| 0.6.0 | 浏览器插件化、OSS 更新渠道与稳定性修复 | ✅ |
| 0.8.3 | 浏览器缩放与开发体验优化 | ✅ |
| 0.9.0 | 嵌入式终端与消息列表导航交互 | ✅ |
| 0.9.1 | 终端稳定性与 OpenAI Chat Completions 修复 | ✅ |
| 0.10.0 | 工作区标签页、文档解析与 OpenAI Responses | ✅ |
| 0.10.1 | 发布后异常修复：Agent 协作与运行时稳定性 | ✅ |
| 0.10.2 | Hotfix：DeepSeek reasoning_content、浏览器标签与 MCP 体验修复 | ✅ |
| 0.11.0 | ReAct Loop 与总结阶段分离、会话列表分组、工具输出截取与执行时间持久化 | ✅ |
| 0.12.0 | CLI 模块化配置增强：Prompt/Server/Model/Memory/doctor + Linux 服务器支持 | 🚧 进行中 |

## 当前执行策略（2026-06-29）

- Phase 1~21 已完成。
- 0.11.0 已从 `main` 发布，版本标签为 `v0.11.0`。
- 当前主线为 **0.12.0：CLI 模块化配置增强**（Phase 22），目标是让纯服务端（Linux 服务器、Docker、无桌面环境）具备与桌面设置页等价的分模块配置能力。设计方向见 `docs/rfc/0015-cli-modular-config.md`。
- 0.11.0 聚焦 ReAct Loop 与总结阶段分离架构重构、左侧会话列表按 workspace 分组、工具输出截取与执行时间持久化、双总结修复与智能提升最终回复、终端面板聚焦修复、ReAct 循环失败错误痕迹持久化，以及扁平 Message 语义构造器与前端 reasoning 空值保护。
- Phase 21-M 统一工作区 Tabs 与会话绑定已完成，不再作为当前开发主线。
- 0.10.0 已交付内容包括工作区标签页与会话绑定、Office/PDF 文件上传与本地脚本解析、OpenAI Responses provider 与 Chat Completions 拆分、插件能力 Plugin::register 自注册、统一 Agent 交互通道（AgentInput trait）及若干终端与 OpenAI 流式修复。

## 发布流程规范

- 发布分支必须从最新 `origin/main` 创建，建议使用 worktree 独立迁出，分支命名为 `release/x.y.z`。
- 在发布分支中只处理版本号、发布配置、必要修复和发布前验证，不混入后续功能开发。
- 发布分支验证通过后，先通过 PR 或直接合并进入 `main`，再基于合并后的 `main` 最终提交创建版本标签。
- 版本标签必须标记在 `main` 的最终发布提交上，再推送标签触发发布流程，避免标签停留在发布分支侧线上。
- 发布前如果有紧急修复，先合入发布分支并重新验证，再合并到 `main`，最后在 `main` 上打标签。
- 已推送并触发打包的版本标签原则上不强推、不移动，避免重复打包或覆盖既有发布产物。

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

## Phase 21：内嵌浏览器面板（已完成）

> Issue：#95

基于 Tauri 2 多 WebView 能力，在主窗口内嵌入浏览器面板，用户和 Agent 共享浏览器会话。用户可正常浏览和登录，Agent 通过 Bridge Script 操控页面。

**核心能力**：真实浏览器渲染、用户直接操作、Agent 自动操控、人机协作、Cookie 持久化。

| 子阶段 | 内容 | 状态 |
|--------|------|------|
| 21-A | 技术验证 — Tauri 多 WebView、initialization_script、IPC 通信、位置跟随 | ✅ |
| 21-B | BrowserManager 核心 — browser.rs、Bridge Script、Tauri Commands、Agent web_fetch 拦截 | ✅ |
| 21-C | 前端浏览器面板 — BrowserPanel 组件、地址栏、容器同步、拖拽调整宽度 | ✅ |
| 21-D | Agent 交互增强 — ObservePage 快照、web_browse 工具、页面内容注入对话链 | ✅ |
| 21-E | 表单填写增强 — 三层策略（keyboard / native setter / paste）、表单提取、模拟点击、加载HTML | ✅ |
| 21-F | 完善与优化 — Cookie 持久化、权限审批、导航控制 | ✅ |
| 21-H | UI 库适配与框架检测 — detectFramework、UI 库组件提取和填写 | ✅ |
| 21-I | 多标签页支持 — 单渲染器 + 标签状态管理、前端标签栏 | ✅ |
| 21-J | 用户批注模式 — canvas 覆盖层绘制、Agent 读取批注 | ✅ |
| 21-G | 浏览器能力插件化 — Core trait 抽象、Tauri Plugin 封装、应用层切换 | ✅ |

## Phase 22：CLI 模块化配置体系（进行中）

> RFC：`docs/rfc/0015-cli-modular-config.md`

让纯服务端环境具备与桌面设置页等价的分模块配置能力。Desktop 优先不变，CLI 与 Desktop 共享同一份 `~/.tiangong/` 配置。不做 `tiangong init` 一站式向导，每类配置由独立命令管理。

**设计原则**：

- 不做 `init`，不把模型/Server/Memory/MCP/Prompt 混在一个流程。
- 每类配置独立命令，用户按需配置。
- CLI 与 Desktop 共享同一份配置目录。
- `doctor` 提供诊断，不强制初始化。

| 子阶段 | 内容 | 状态 |
|--------|------|------|
| 22-A | 文档与 Roadmap — RFC 0015、PLAN.md、TODO.md、Linux 服务器部署指南 | 🚧 |
| 22-B | Prompt 独立配置 — `custom-prompt.md` 独立存储 + 加载优先级兼容 + `prompt show/set/edit/clear/path` | ⬜ |
| 22-C | Server 配置 CLI — `server config/token/status` + ServerConfig 统一 + Token 生成 | ⬜ |
| 22-D | 模型配置 CLI — Provider/Model/Routing 增删改查 + `model validate/test` 连通性测试 | ⬜ |
| 22-E | Memory 配置 CLI — `memory config/enable/disable/status/test` + 标记文件开关 | ⬜ |
| 22-F | config + doctor — `config path/show/validate` + `doctor` 聚合诊断 | ⬜ |

**关键决策**：

- 自定义 Prompt 写入 `custom-prompt.md` 时清空 app.json 旧字段，使 .md 成为唯一事实来源。
- Memory enable/disable 用轻量标记文件 `~/.tiangong/memory/.disabled`，不破坏 MemoryConfig 结构。
- ServerConfig 统一到 `tiangong-config` 版本，消除两套同名结构的技术债。
- API Key 复用现有 `${VAR}` 模板机制处理 `--api-key-env` / `--api-key`。

## 参考文档

- 架构基准：`docs/desktop-agent-technical-architecture.md`
- 需求基线：`docs/requirements.md`
- Server API：`docs/server-api.md`
- RFC 0011：`docs/rfc/0011-multi-agent-collaboration.md`
- RFC 0015（CLI 模块化配置）：`docs/rfc/0015-cli-modular-config.md`
- Linux 服务器部署：`docs/linux-server-deployment.md`
