# TODO - 天工全栈平台重构任务清单

> 最后更新：2026-03-23
> 当前主线 RFC：`docs/rfc/0004-full-stack-agent-platform.md`
> 参考：`PLAN.md`、`docs/requirements.md`

---

## Phase 2：Skill 管理 MVP（已完成 ✅）

- [x] Skill 支持安装、启停、卸载、列表、详情
- [x] `/skill` 管理交互对齐 `/mcp`
- [x] 动态 Step 执行闭环
- [x] Skill 锁文件（skills-lock.json / mcp-lock.json）
- [x] MCP 托管映射（卸载 skill 时自动清理无引用的托管 MCP server）
- [x] 事务回滚（安装失败时 RAII 自动回滚已复制文件）
- [x] 审计日志（~/.tiangong/audit.jsonl，记录 skill/mcp 操作）

---

## Phase 3：Workspace 拆分与核心抽离（已完成 ✅）

### A. Workspace 基础结构

- [x] 创建 `crates/` 目录与 workspace Cargo.toml
- [x] 新建 `crates/tiangong-core/Cargo.toml`，迁移核心依赖
- [x] 新建 `crates/tiangong-cli/Cargo.toml`，CLI 依赖
- [x] 新建 `crates/tiangong-entry/Cargo.toml`，命令路由
- [x] 调整主 `Cargo.toml` 为 workspace 根配置
- [x] 创建空骨架 crate：tiangong-server/gateway/connector/media

### B. 核心引擎迁移 (tiangong-core)

- [x] 迁移 `src/core/*` → `crates/tiangong-core/src/`
- [x] 路径替换 `crate::core::` → `crate::`
- [x] 新增 `context/` 上下文管理模块（压缩器 + 组织器）
- [x] 新增 `plugin/` 插件框架（类型 + 注册表 + 生命周期）
- [x] 修复原有编译错误（model.rs 类型导入、lite_model、tracing 宏）
- [x] 修复可见性问题（pub(in crate::app_state) → pub）
- [x] 确保 `tiangong-core` 可独立编译，无 UI 依赖

### C. CLI 前端 (tiangong-cli)

- [x] Codex 风格 REPL（对话自然滚动，终端原生文本选择）
- [x] 交互式输入（crossterm raw mode 行编辑、历史导航）
- [x] `/` 命令补全 + `@` 提及补全（自动弹出竖排候选列表）
- [x] ratatui modal 管理界面（/sessions、/mcp、/skill）
- [x] 命令系统（/new、/model、/config、/cancel、/help）
- [x] 草稿会话（启动不创建空会话，首次发送才记录）

### D. GUI 前端（src-tauri）

- [x] src-tauri 保持独立 Tauri workspace
- [x] 路径更新 `tiangong_core::core::` → `tiangong_core::`
- [x] 编译验证通过

### E. 主入口统一 (tiangong-entry)

- [x] 迁移 `src/entry/*` → `crates/tiangong-entry/src/`
- [x] 路径替换 + 可见性调整
- [x] CLI 命令恢复：`Some(MainCommand::Cli)` → `tiangong_cli::run_cli()`

### F. 验证与交付

- [x] `cargo fmt -- --check` 通过
- [x] `cargo check --workspace` 通过
- [x] `cargo clippy --workspace --all-targets --tests --benches -- -D warnings` 通过
- [x] CLI 功能可用（REPL + 命令 + modal 管理）
- [x] src-tauri 编译通过
- [x] 死代码清理（src/ui/、src/cli/、src/lib.rs、src/core/、src/entry/）

---

## 后续阶段（待展开）

### Phase 4：Server 模式（已完成 ✅）
- [x] 新建 `crates/tiangong-server`
- [x] 实现 REST API（/chat /sessions /mcp /skills /health /shutdown）
- [x] 实现 WebSocket 流式通信（/api/v1/ws，EventBus 事件推送）
- [x] API Token 认证（Bearer Token）
- [x] `-d` / `--daemon` 后台运行支持（PID 文件管理）
- [x] `tiangong server stop` 停止后台 Server
- [x] Server 启动时自动加载已启用 Connector（connectors.json）

### Phase 5：Gateway 与事件总线（已完成 ✅）
- [x] 实现 EventBus（tokio broadcast，会话/Agent/Connector/系统事件）
- [x] 实现 Gateway 消息路由（MessageRouter）
- [x] 统一消息模型（IncomingMessage/OutgoingMessage/MessageContent）

### Phase 6：Connector 框架（已完成 ✅）
- [x] 定义 Connector trait（async start/stop/send_message/health_check）
- [x] 实现 Webhook Connector（最小实现）
- [x] ConnectorManager（注册/启停/健康检查）
- [x] ConnectorConfig/ConnectorType（Webhook/Telegram/Discord/Lark）
- [x] 实现 Telegram Connector（teloxide，feature gate `telegram`）
- [x] 实现 Discord Connector（serenity，feature gate `discord`）
- [x] 实现飞书/Lark Connector（reqwest HTTP API，feature gate `lark`）

### Phase 7：多媒体能力（框架已完成 ✅）
- [x] ImageGenerator trait + 请求/响应类型
- [x] VideoGenerator trait + 异步任务类型
- [x] SpeechRecognizer trait（STT）
- [x] SpeechSynthesizer trait（TTS）+ VoiceInfo
- [x] MediaTask 异步任务管理类型
- [x] 图片生成后端实现（OpenAI DALL-E / GPT-Image，reqwest）
- [x] 视频生成占位（StubVideoGenerator，待 Sora/Kling API 稳定后接入）
- [x] 语音识别后端实现（OpenAI Whisper，multipart 上传）
- [x] 语音合成后端实现（OpenAI TTS，6 个预定义音色）
- [x] MediaAgent 聚合器（builder 模式，能力检查）
- [x] Connector 语音消息自动转文字（Gateway MessageRouter 自动 STT）

### Phase 8：生产化与完善（已完成 ✅）
- [x] 日志分级（支持 `TIANGONG_LOG` 环境变量按模块调级别）
- [x] 错误恢复（启动时 recover_interrupted_tasks + auto_resume_unfinished_plan）
- [x] 敏感配置脱敏（model auth_token + server auth_token 脱敏显示）
- [x] 配置热重载（ConfigWatcher 轮询文件修改时间 + Notify 通知）

---

## Phase 9：模型配置重构与多媒体集成 — **当前阶段**

### A. 模型配置重构（已完成 ✅）
- [x] ModelsConfig 替换 ModelProviderConfig 为唯一模型配置源
- [x] Provider + Model + Routing 三层架构
- [x] ModelsConfig.to_chat_provider_config() 自动生成内部配置
- [x] 移除旧版 draft 字段和 legacy API
- [x] 设置页面自动保存（debounce 500ms）
- [x] 选择 Provider 后可获取模型列表快速配置
- [x] 模型标识名自动生成

### B. GUI 升级（已完成 ✅）
- [x] shadcn/ui dashboard 风格重构（Sidebar + Header + Tabs + Card + Select）
- [x] 暗/亮/跟随系统主题切换
- [x] macOS 红绿灯对齐（trafficLightPosition）
- [x] 消息展示优化（系统消息分组折叠、间距缩减、文字对比度）
- [x] 输入框嵌入式发送按钮
- [x] 会话标题 LLM 自动生成
- [x] 空会话过滤 + 会话列表按更新时间倒序

### C. 执行优化（已完成 ✅）
- [x] 意图分类快速路径：简单对话跳过 planning + execution
- [x] poll_pending_turn 修复（消息回复链路）
- [x] 完成后状态重置为 idle

### D. 上下文摘要压缩（已完成 ✅）
- [x] ContextCompressor 重写：LLM 摘要优先，滑动窗口回退
- [x] ContextOrganizer 集成到 RuntimeEngine，替代硬编码滑动窗口
- [x] ReAct 循环内 loop_messages 累积压缩（基于 API 返回的精确 prompt_tokens 触发）
- [x] 双重 token 判断：首次用字符估算预判，后续用 API 精确 prompt_tokens 驱动

### E. 多媒体能力集成到执行引擎（已完成 ✅）

#### E1. 图片生成集成
- [x] `generate_image` 工具注入（prompt + width/height/style 可选参数）
- [x] `handle_media_generation` 调用 OpenAI 兼容 API 生成图片
- [x] 返回 Markdown 图片语法，前端 ReactMarkdown 自动渲染
- [x] 前端图片最大宽度限制 + 点击全屏放大

#### E2. 语音合成/识别集成
- [x] `text_to_speech` 工具注入（text + voice/speed/output_path 参数）
- [x] 调用 OpenAI TTS 后端，音频保存到 `~/.tiangong/media/`
- [x] `speech_to_text` 工具注入（file_path + language 参数）
- [x] 调用 OpenAI Whisper 后端，返回转录文本

#### E3. 视频生成
- [x] 视频生成通过 Skill 机制处理（无稳定 API，不内置）

### F. 验证
- [x] `cargo check --workspace` 通过
- [x] `cargo clippy --workspace --all-targets --tests --benches -- -D warnings` 通过
- [x] 前端 `yarn build` 通过
