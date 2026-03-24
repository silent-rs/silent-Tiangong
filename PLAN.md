# 天工项目规划（PLAN）

## 愿景
构建一个全功能可扩展的 GUI + CLI + Server 个人智能终端平台，实现"可对话、可规划、可执行、可扩展、可治理、可远程"的 Agent 能力闭环。通过 Connector 机制接入各类 IM 通道，支持图片/视频等多媒体生成，打造个人 AI 中枢。

## 总体目标
- 产品目标：从桌面对话工具演进为全栈个人智能终端（本地 + 远程 + 多通道）。
- 架构目标：Workspace 多 crate 分离，核心引擎、前端、Server、Connector、媒体生成各自独立。
- 安全目标：Server 模式默认安全（本地绑定、Token 认证），Connector 鉴权白名单制。
- 工程目标：按 Phase 增量交付，每个 Phase 保证功能不回退。

## 当前执行策略（2026-03-20）
- 架构 RFC：`docs/rfc/0004-full-stack-agent-platform.md`
- 前置基线：Phase 1（CLI Agent）已达成，Phase 2（Skill 管理）部分完成。
- 当前目标：完成 Workspace 拆分，为后续 Server/Connector/Media 能力打基础。

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

### Phase 9（模型配置重构与多媒体集成）— **当前阶段**
- `ModelsConfig` 替换 `ModelProviderConfig` 为唯一模型配置源。
- Provider + Model + Routing 三层架构。
- GUI 全面升级为 shadcn/ui dashboard 风格，支持暗/亮主题。
- 意图分类快速路径：简单对话跳过 planning，减少 token 消耗。
- 多媒体能力通过 Routing 集成到执行引擎：
  - 图片生成作为内置工具，调用 routing 中配置的 image_generation 模型。
  - 语音合成/识别作为内置工具，调用 routing 中配置的 tts/stt 模型。
- MediaAgent 从 ModelsConfig routing 自动初始化。

## 参考文档
- 项目说明：`README.md`
- RFC 0001：`docs/rfc/0001-tiangong-desktop-agent-roadmap.md`
- RFC 0002：`docs/rfc/0002-cli-agent-roadmap.md`
- RFC 0003：`docs/rfc/0003-skill-market.md`
- RFC 0004：`docs/rfc/0004-full-stack-agent-platform.md`（全栈平台架构）
- 需求基线：`docs/requirements.md`
