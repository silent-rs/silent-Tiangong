# 天工需求整理

## 文档目的
用于对齐天工全栈平台重构（RFC 0004）的开发边界，作为 `PLAN.md`、`TODO.md` 与实现代码的一致性基线。

## 已有基线能力

以下能力视为当前稳定基线，重构过程中必须保持不回退：

- `tiangong` 默认保持桌面 GUI 入口（Tauri + React + shadcn/ui），CLI 入口为 `tiangong cli`。
- CLI 具备任务闭环：输入 -> planning -> executing -> final response。
- MCP 已支持本地 `stdio` 与远程 HTTP（JSON-RPC over HTTP）并存。
- Agent 配置已支持查看、更新、校验与即时生效。
- Skills 已支持本地安装、启停、卸载与按任务意图匹配。
- 动态 Step 执行闭环已实现。

## 当前范围（RFC 0004：全栈个人智能终端平台）

### Must

#### Workspace 拆分
- 项目必须从单 crate 重构为 Cargo workspace 多 crate 结构。
- `tiangong-core` 必须作为独立 crate，不依赖任何 UI 框架。
- `tiangong-cli` 和 `tiangong-gui` 必须作为独立前端 crate，依赖 `tiangong-core`。
- 重构后现有 GUI 和 CLI 功能必须完整保留，不允许功能回退。

#### 模型配置
- 模型配置必须独立为 `models.json`，采用 Provider 与 Model 分离设计。
- Provider 层只定义连接信息（`base_url`、`api_key`、`timeout_ms`），可被多个模型共享。
- Model 层引用 Provider，声明实际模型 ID 和能力列表（`capabilities`），携带专属参数（`options`）。
- 能力类型包括：`chat`（常规对话/推理）、`multimodal`（多模态理解）、`image_generation`（图片生成）、`video_generation`（视频生成）、`stt`（语音识别）、`tts`（语音合成）。
- 一个模型可声明多种能力（如 gpt-4o 同时支持 chat 和 multimodal）。
- 必须通过 `routing` 表为每种能力指定默认使用的模型。
- `chat` 为基础必选能力，routing 中未配置 `chat` 时程序必须在所有模式（GUI/CLI/Server）下持续提示用户完成模型设置，在设置完成前不执行对话任务。
- 其余能力（multimodal/image_generation/video_generation/stt/tts）未在 routing 中配置时视为关闭，对应功能不可用但不影响其他功能正常运行。
- `api_key` 必须支持环境变量引用（`${ENV_VAR}` 语法），避免明文存储。

#### Server 模式
- 必须新增 `tiangong server` 命令，启动 HTTP REST + WebSocket 服务。
- 必须支持 `-d` / `--daemon` 参数后台运行（fork 进程后主进程退出，PID 写入 `~/.tiangong/server.pid`）。
- 必须支持 `tiangong server stop` 命令，读取 PID 文件停止后台 Server 进程。
- Server 启动时必须自动加载并启动所有已启用的 Connector。
- REST API 必须覆盖：对话、会话管理、Skill/MCP 管理。
- WebSocket 必须支持流式对话与事件推送。
- Server 模式必须支持 API Token 认证。
- 默认绑定 `127.0.0.1`，需用户显式指定才可绑定外部地址。

#### Connector 机制
- 必须定义标准化 Connector trait，支持消息收发、媒体传输、健康检查。
- 必须实现统一消息模型（IncomingMessage / OutgoingMessage），支持文本、图片、文件、音频、视频。
- 首批必须支持至少 3 种 IM 通道（Telegram + 飞书/Lark + Webhook）。
- Connector 必须支持配置化管理（启停、凭据配置、白名单）。
- 通过 Connector 接收的消息必须走与本地 CLI 相同的 Agent 执行链路。

#### 多媒体能力
- 必须支持图片生成能力，至少实现一个后端适配（OpenAI DALL-E / GPT-Image）；models.json 中未配置 `image_generation` routing 时该功能自动关闭。
- 必须支持视频生成能力，至少实现一个后端适配；未配置 routing 时自动关闭。
- 必须支持语音识别（STT）能力，至少实现 OpenAI Whisper 适配；未配置 routing 时自动关闭。
- 必须支持语音合成（TTS）能力，至少实现 OpenAI TTS 适配；未配置 routing 时自动关闭。
- 媒体生成必须采用异步任务模型，支持状态查询。
- 生成的媒体文件必须可通过 Connector 发送到 IM 通道。
- Connector 接收到语音消息时，必须支持自动调用 STT 转为文字后交给 Agent 处理。
- Agent 响应时应支持可选的 TTS 输出，将文本回复转为语音发送。

### Should

- Server 模式应支持 CORS 配置，方便 Web 前端调用。
- 应实现事件总线（EventBus）实现各层解耦通信。
- 应支持 Discord Bot Connector。
- 应支持钉钉/Slack Connector。
- 应支持配置热重载，修改配置无需重启。
- 多媒体生成应支持 Stable Diffusion / Flux 等开源模型后端。
- Agent 层应集成 MediaAgent，支持在对话中自然触发图片/视频生成。
- 应支持 Docker 部署，提供 Dockerfile 和 docker-compose 模板。

### 非目标（当前阶段不做）

- 多用户权限与租户隔离体系。
- 商业化支付与计费。
- 公共 Skill 市场与评分系统。
- 微信接入（API 限制大，后续单独评估）。
- 多用户权限与租户隔离。
- 商业化计费。
- P2P 分发与去中心化网络。

## 参考

- `README.md`
- `PLAN.md`
- `TODO.md`
- `docs/rfc/0004-full-stack-agent-platform.md`
