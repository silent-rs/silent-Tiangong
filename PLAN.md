# 天工项目规划（PLAN）

## 愿景
构建一个全功能可扩展的 GUI + CLI + Server 个人智能终端平台，实现"可对话、可规划、可执行、可扩展、可治理、可远程"的 Agent 能力闭环。通过 Connector 机制接入各类 IM 通道，支持图片/视频等多媒体生成，打造个人 AI 中枢。

## 总体目标
- 产品目标：从桌面对话工具演进为全栈个人智能终端（本地 + 远程 + 多通道）。
- 架构目标：Workspace 多 crate 分离，核心引擎、前端、Server、Connector、媒体生成各自独立。
- 安全目标：Server 模式默认安全（本地绑定、Token 认证），Connector 鉴权白名单制。
- 工程目标：按 Phase 增量交付，每个 Phase 保证功能不回退。

## 当前执行策略（2026-04-13）
- 架构 RFC：`docs/rfc/0004-full-stack-agent-platform.md`
- 架构基准：`docs/desktop-agent-technical-architecture.md`
- 差距分析：`docs/architecture-gap-analysis.md`
- Phase 1~14 主体已完成，GUI 手工验证剩余收尾项。
- 当前目标：抽离 LLM 协议层，新增 Anthropic Messages 请求支持，避免对话链路继续绑定 OpenAI 兼容协议。

## 里程碑

### Phase 1（CLI Agent 基线，已达成）
- 单 Agent 对话能力可用。
- 最小任务执行链路可跑通（输入 -> 规划 -> 执行 -> 反馈）。
- MCP 本地/远程接入可用。

### Phase 2（Skill 管理 MVP，部分完成）
- Skill 支持安装、启停、卸载、列表、详情。
- `/skill` 管理交互对齐 `/mcp`。
- 动态 Step 执行闭环。
- 待完成：锁文件、MCP 托管映射、事务回滚、审计。

### Phase 3（Workspace 拆分与核心抽离）— **当前阶段**
- 单 crate → Cargo workspace 多 crate。
- `tiangong-core`：核心引擎独立（无 UI 依赖）。
- `tiangong-cli`：CLI/TUI 前端独立。
- `tiangong-gui`：桌面 GUI 前端独立（Tauri + React）。
- 主二进制统一入口分发。
- 确保现有功能不回退。

### Phase 4（Server 模式）
- `tiangong-server`：HTTP REST + WebSocket API。
- 对话、会话管理、Skill/MCP 管理 API。
- API Token 认证与访问控制。
- `tiangong server` 命令启动，支持 `-d` / `--daemon` 后台运行。
- `tiangong server stop` 停止后台运行的 Server。
- Server 启动时自动加载并启动已启用的 Connector。

### Phase 5（Gateway 与事件总线）
- 事件总线（EventBus）实现层间解耦。
- Gateway 统一消息路由。
- 统一消息模型（IncomingMessage / OutgoingMessage）。

### Phase 6（Connector 框架与 IM 接入）
- `tiangong-connector`：Connector trait 定义。
- 首批适配器：Telegram、Discord、飞书/Lark、Webhook。
- Connector 配置管理与热插拔。
- 后续扩展：钉钉、Slack 等。

### Phase 7（多媒体能力）
- `tiangong-media`：图片/视频生成 + 语音识别/合成框架。
- 图片生成后端：OpenAI DALL-E / GPT-Image、Flux。
- 视频生成后端：Sora、Kling。
- 语音识别后端：OpenAI Whisper、讯飞。
- 语音合成后端：OpenAI TTS、ElevenLabs。
- Agent 层集成 MediaAgent。
- Connector 支持语音消息自动转文字。

### Phase 8（生产化与完善）
- 日志与监控完善。
- 配置热重载。
- TLS、安全加固、Docker 部署支持。

### Phase 9（模型配置重构与多媒体集成，已完成）
- `ModelsConfig` 替换 `ModelProviderConfig` 为唯一模型配置源。
- Provider + Model + Routing 三层架构。
- GUI 全面升级为 shadcn/ui dashboard 风格，支持暗/亮主题。
- 意图分类快速路径：简单对话跳过 planning，减少 token 消耗。
- 多媒体能力通过 Routing 集成到执行引擎。

### Phase 10（友好交互改造）— 进行中
- GUI 样式简化（去头像、去气泡边框）。
- GUI 解释文本独立流式展示。
- CLI 实时流式展示。

### Phase 11（架构补全 — 运行时基础设施）— **当前阶段**
> 对照 `docs/desktop-agent-technical-architecture.md` 补全缺失能力
> 差距分析：`docs/architecture-gap-analysis.md`

**Phase 11-A：基础设施（高优先级）**
- 统一任务模型：合并 RunStatus / TaskStatus 为 UnifiedTaskStatus，覆盖完整状态机。
- 查询编排层独立抽象：新建 `orchestrator/` 模块，扩展 QueryMode 为多路由。

**Phase 11-B：执行闭环（高优先级）**
- 后台任务回流：后台任务完成 → RuntimeEvent → EventBus → 会话注入。
- 恢复与持久化：任务状态实时持久化，启动时恢复未完成任务现场。

**Phase 11-C：能力增强（中优先级）**
- 上下文装配层增强：用户偏好/长期记忆注入，预留检索接口。
- 多代理 Worker 隔离：独立工具集、上下文边界、预算上限。
- 权限细粒度控制：路径级规则、网络目标限制。
- 观测与成本治理：请求级/任务级/会话级三层成本聚合。

**Phase 11-D：远程能力（低优先级）**
- 远程接入角色模型：控制者/审批者/观察者角色区分。

### Phase 12（事件驱动循环运行时）— **当前阶段**
> RFC：`docs/rfc/0005-event-loop-runtime.md`

将运行时从 Turn-based 改为 Event-loop 模型：
- Phase A：EventLoopRunner 核心 + 挂起/恢复
- Phase B：ActiveLoops 管理器 + LoopHost trait
- Phase C：生命周期管理、优雅关闭与持久化恢复
- Phase D：清理旧代码（TurnRunner / QueryClassifier / ControlSignal）

### Phase 13（CoreConfig 配置注入）— **待启动**
> RFC：`docs/rfc/0006-core-config-provider.md`

将 TiangongCore 的配置从内部磁盘加载改为外部注入：
- Phase A：定义 CoreConfig + CoreConfigProvider，修改 TiangongCore 构造函数
- Phase B：CLI/GUI/Server 适配，使用 CoreConfigProvider 注入配置
- Phase C：tiangong-config 独立 crate（可选），提供磁盘加载、持久化、文件监听

### Phase 15（LLM 协议抽象与 Anthropic 支持）— **进行中**
- Provider 配置增加协议类型，区分 OpenAI 兼容与 Anthropic。
- 新增独立 crate `tiangong-llm`，统一承载 Provider 抽象、领域模型、错误类型与各协议适配。
- 在 `tiangong-core` 内部维护统一 Provider 抽象和领域模型，Anthropic provider 先通过 `async-anthropic` 适配，保证未来可替换为原生 `reqwest + serde` 实现。
- `async-openai` 也迁入 `tiangong-llm` 作为 OpenAI 兼容 provider，避免协议实现继续散落在 `tiangong-core`。
- 对话、流式输出、工具调用、轻量模型调用补齐 Anthropic Messages 适配。
- GUI / CLI / Server 共享同一套协议能力判断与错误处理。

## 参考文档
- 项目说明：`README.md`
- RFC 0001：`docs/rfc/0001-tiangong-desktop-agent-roadmap.md`
- RFC 0002：`docs/rfc/0002-cli-agent-roadmap.md`
- RFC 0003：`docs/rfc/0003-skill-market.md`
- RFC 0004：`docs/rfc/0004-full-stack-agent-platform.md`（全栈平台架构）
- RFC 0006：`docs/rfc/0006-core-config-provider.md`（CoreConfig 配置注入）
- 架构基准：`docs/desktop-agent-technical-architecture.md`
- 架构差距分析：`docs/architecture-gap-analysis.md`
- 需求基线：`docs/requirements.md`
