# TODO - 天工全栈平台重构任务清单

> 最后更新：2026-04-03
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

---

## Phase 10：友好交互改造 — **当前阶段**

### A. GUI 样式简化（已完成 ✅）
- [x] 去掉 Assistant/User 头像图标
- [x] 去掉消息气泡边框和背景色
- [x] 调整因头像而存在的间距
- [x] 审批请求和思考中指示器同步去掉头像

### B. GUI 解释文本独立流式展示（已完成 ✅）
- [x] useStore 新增流式系统消息追踪状态
- [x] SystemMessageGroup 改造：活跃 round 以流式文本块展示（不折叠）
- [x] 工具调用消息保持现有折叠展示方式不变

### C. CLI 实时流式展示（已完成 ✅）
- [x] repl.rs 轮询循环从阻塞等待改为边轮询边输出增量
- [x] output.rs 新增流式输出函数（解释文本、工具摘要、增量输出）
- [x] 追踪消息增量，实现系统消息和助手消息的实时展示

### D. 验证
- [x] `cargo check --workspace` 通过
- [x] `cargo clippy --workspace --all-targets --tests --benches -- -D warnings` 通过
- [x] 前端 `yarn build` 通过

---

## Phase 11：架构补全 — 运行时基础设施 — **当前阶段**

> 差距分析文档：`docs/architecture-gap-analysis.md`
> 基准文档：`docs/desktop-agent-technical-architecture.md`

### Phase 11-A：基础设施（高优先级）

#### A1. 统一任务模型（GAP-5）
- [x] 新建 `src/task/mod.rs` 模块入口
- [x] 新建 `src/task/model.rs`：定义 `UnifiedTask` 结构和 `UnifiedTaskStatus` 枚举
  - 状态：Queued / Running / Blocked / WaitingApproval / Backgrounded / Completed / Failed / Cancelled
  - 字段：id / input_summary / agent_id / progress / result_location / session_id / work_dir / created_at / updated_at
- [x] 新建 `src/task/state_machine.rs`：状态转换验证（只允许合法转换）
- [x] 新建 `src/task/registry.rs`：统一任务注册表（替代 BackgroundTask 的独立 registry）
- [x] RunStatus 保留为 UI 展示层状态（与 UnifiedTaskStatus 不同层面概念）
- [x] TaskStatus ↔ UnifiedTaskStatus 互转实现（From trait）
- [x] `lib.rs` 注册 `task` 模块
- [x] 验证：`cargo check --workspace` 通过

#### A2. 查询编排层独立抽象（GAP-1 + GAP-3）
- [x] 新建 `src/orchestrator/mod.rs` 模块入口
- [x] 新建 `src/orchestrator/types.rs`：扩展 `QueryMode` 枚举
  - DirectAnswer / SingleToolExecution / MultiStepExecution / TaskSplit / BackgroundExecution
- [x] 新建 `src/orchestrator/query_orchestrator.rs`：`QueryOrchestrator` 控制中心
  - 接受事件 → 判断打断 → 路由决策
  - LLM 分类器判断执行模式
- [x] 修改 `turn_runner.rs`：Init 阶段默认使用 MultiStepExecution
- [x] 修改 `context/assembler.rs`：QueryMode 重导出自 orchestrator，使用 needs_tools() 统一判断
- [x] `lib.rs` 注册 `orchestrator` 模块
- [x] 验证：`cargo check --workspace` 通过

### Phase 11-B：执行闭环（高优先级）

#### B1. 后台任务回流与通知（GAP-6）
- [x] 新建 `src/task/notification.rs`：任务完成通知机制
  - 任务完成 → 生成 `RuntimeEvent`（TaskCompleted/TaskFailed）
  - 通过 channel（TaskNotificationBus）发布
- [x] 后台任务回流通过 LoopEvent::BackgroundTaskDone 在 EventLoop 中处理
- [x] 当前通过 query_task 工具拉取结果（推模式后续扩展）
- [x] 验证：`cargo check --workspace` 通过

#### B2. 恢复与持久化增强（GAP-9）
- [x] 新建 `src/task/persistence.rs`：任务状态持久化
- [x] 新建 `src/task/recovery.rs`：启动恢复逻辑
- [x] 修改 `app_state/facade/lifecycle.rs`：启动时加载并清理持久化的 EventLoop 状态
- [x] 验证：`cargo check --workspace` 通过

### Phase 11-C：能力增强（中优先级）

#### C1. 上下文装配层增强（GAP-2）
- [x] 新建 `src/context/memory.rs`：用户偏好与长期记忆
- [x] 新建 `src/context/retriever.rs`：检索接口（预留 trait）
- [x] EventLoopRunner::call_llm 首轮注入记忆上下文
- [x] 验证：`cargo check --workspace` 通过

#### C2. 多代理 Worker 隔离增强（GAP-4）
- [x] 修改 `src/coordinator/types.rs`：WorkerBudget 增加 max_tool_calls，WorkerContext 增加 is_tool_allowed()
- [x] 修改 `src/coordinator/worker.rs`：执行完成后检查 token/时长预算超限并记录警告
- [x] 修改 `src/coordinator/task_coordinator.rs`：多 Worker 模式注入独立预算（轮次/工具/时长限制）
- [x] 验证：`cargo check --workspace` 通过

#### C3. 权限细粒度控制（GAP-7）
- [x] 修改 `src/permission.rs`：PathRule + NetworkRule
- [x] 新增 `PermissionGate::check_path()` 和 `check_network()`
- [x] 修改 `src/observe/audit.rs`：AuditRecord 增加 args_summary 字段和 with_args_summary()
- [x] 验证：`cargo check --workspace` 通过

#### C4. 观测与成本治理闭环（GAP-10）
- [x] 修改 `src/observe/cost.rs`：新增 RequestCost / TaskCost / SessionCost 三层
- [x] 新建 `src/observe/collector.rs`：统一采集入口 ObserveCollector
- [x] ObserveCollector 集成到 EventLoopRunner，每次 LLM 调用自动记录成本
- [x] 验证：`cargo check --workspace` 通过

### Phase 11-D：远程能力（低优先级）

#### D1. 远程接入角色模型（GAP-8）
- [x] 新建 `tiangong-gateway/src/role.rs`：RemoteRole 枚举（Controller/Approver/Observer）
- [x] 修改 `tiangong-gateway/src/message.rs`：IncomingMessage 增加 sender_role
- [x] 修改 `tiangong-gateway/src/router.rs`：handle_incoming 入口根据角色限制操作
- [x] 验证：`cargo check --workspace` 通过

### Phase 11-E：最终验证（已完成 ✅）
- [x] `cargo fmt -- --check` 通过
- [x] `cargo check --workspace` 通过
- [x] `cargo clippy --workspace --all-targets --tests --benches -- -D warnings` 通过
- [x] `cargo nextest run --workspace --no-tests pass` 通过

---

## Phase 12：事件驱动循环运行时 — **当前阶段**

> RFC：`docs/rfc/0005-event-loop-runtime.md`

### Phase 12-A：EventLoopRunner 核心 + 挂起/恢复（已完成 ✅）

- [x] 新建 `src/event_loop/mod.rs` 模块入口
- [x] 新建 `src/event_loop/types.rs`：LoopEvent / LoopPhase / LoopState 类型定义
- [x] 新建 `src/event_loop/runner.rs`：EventLoopRunner 核心循环
- [x] 新建 `src/event_loop/context.rs`：事件到上下文的转换逻辑
- [x] `lib.rs` 注册 `event_loop` 模块
- [x] 验证：`cargo check --workspace` 通过

### Phase 12-B：LoopHost trait + ActiveLoops 管理器（已完成 ✅）

- [x] 新建 `src/event_loop/host.rs`：LoopHost trait
- [x] 新建 `src/event_loop/active_loops.rs`：MultiLoopHost（多会话管理）
- [x] 新建 `src/event_loop/cli_host.rs`：CliLoopHost（单会话）
- [x] 验证：`cargo check --workspace` 通过

### Phase 12-C：生命周期管理与持久化（已完成 ✅）

- [x] 新建 `src/event_loop/persistence.rs`：PersistedLoopState 读写
- [x] 实现 shutdown_all()：Running→中断保存、Suspended→写盘
- [x] 实现 restore_from_disk()：启动时扫描恢复
- [x] 实现 cleanup_inactive()：超时 loop 写盘移除
- [x] 验证：`cargo check --workspace` 通过

### Phase 12-D：集成与清理

- [x] 修改 `app_state/services/turn/start.rs`：用 EventLoopRunner 替代 TurnRunner/TaskCoordinator
- [x] ControlSignal → LoopEvent 兼容层，poll 逻辑完全复用
- [x] CLI 已通过 TiangongState 间接使用 EventLoopRunner（无需额外改造）
- [x] TurnRunner 仍被 Worker 使用，保留；旧代码后续随 Worker 重构清理
- [x] 验证：完整检查链通过

---

## Phase 13：TiangongCore 纯粹化 + 统一类型 — **当前阶段**

### 已完成

- [x] 新增 `TiangongCore`：单一对话处理核心（sender 推送模式）
  - send_message / cancel / respond_approval / into_session
  - 内部消费线程独占 session，统一事件循环（CoreEvent）
  - EventLoopRunner 空闲时阻塞等待，支持持续输入
- [x] 新增 `tiangong-types` 独立 crate
  - Message / MessageRole / Session / TokenUsage / RunStatus / StreamEvent
  - StreamEvent 带 serde tag JSON 序列化
- [x] Prompt 分层装配系统（prompt/ 模块）
- [x] poll 事件处理统一（process_turn_event）
- [x] GUI send_message 接入 TiangongCore
- [x] CLI 中文 panic 修复（补全 + reasoning 截断）

### 类型迁移（已完成 ✅）

- [x] TokenUsage → `pub use tiangong_types::TokenUsage`
- [x] RunStatus → `pub use tiangong_types::RunStatus`
- [x] StreamEvent → `pub use tiangong_types::StreamEvent`
- [x] Message / MessageRole / now_text → `pub use tiangong_types::{...}`

### 待完成

- [x] GUI emit("stream_event") 直接推送 StreamEvent
- [x] CLI 接入 TiangongCore（直接消费 StreamEvent）
- [x] src-tauri 依赖 tiangong-types
- [x] 删除旧 LoopHost/ActiveLoops/CliLoopHost（-433 行）
- [x] TurnRunner/ControlSignal/EventLoopRunner 已删除（并行智能体集成后清理）
- [x] src-tauri/types.rs DTO 转换层删除，共享消息类型改为直接复用 tiangong-types
- [x] GUI TTS/STT 命令改为复用 tiangong-core 媒体服务，消除对 tiangong-media 的重复直连
- [x] 补充统一能力查询接口（has_model_capability / get_available_capabilities）
- [x] GUI 全链路功能验证通过
- [x] CLI 交互优化（类似 codex/claude code 风格）
- [x] 并行智能体（TaskCoordinator/Worker）集成到 TiangongCore
- [x] 统一用户输入通道（消除消息重复）
- [x] CLI 会话持久化

---

## Phase 13.5：LLM 请求容错 — **当前阶段**

### A. LLM 请求重试机制
- [x] 在 `model.rs` 中实现通用重试包装函数（指数退避，默认 3 次，初始 1s，×2）
- [x] 识别可重试错误类型（429 速率限制、5xx 服务端错误、连接错误）
- [x] 应用到所有 LLM 调用方法（complete / complete_stream / complete_with_functions / complete_with_functions_stream / complete_lite）
- [x] 重试过程通过 `tracing::warn!` 记录日志

### B. 错误与重试通知到前端
- [x] `StreamEvent::Retry` 新增变体，重试时推送到前端
- [x] `StreamEvent::Error` 错误信息写入 session 消息，前端可见
- [x] GUI 前端错误消息红色醒目渲染、重试消息黄色提示渲染
- [x] CLI 重试时输出黄色警告信息
- [x] 底部状态栏显示重试进度和错误详情

### C. 监督模式工具审批 + 工具展示优化
- [x] 监督模式下工具执行前权限检查，Elevated/Critical 工具发送 `ApprovalNeeded` 事件
- [x] Core 阻塞等待用户审批响应，支持允许/拒绝/取消
- [x] 修复 `cancel_turn` 和 `respond_approval` 命令路由到 TiangongCore
- [x] `StreamEvent::ToolStart` 携带 `args_summary`，展示实际命令和参数
- [x] 前端工具展示解析 `工具执行 [xxx]` 格式，折叠摘要包含命令信息
- [x] CLI 审批交互（y/n 确认）

### D. 验证
- [x] `cargo check --workspace` 通过
- [x] `cargo clippy` 通过（tiangong-core / cli / types 零警告）
- [x] `yarn build` 通过

---

## Phase 14：CoreConfig 配置注入 — **当前阶段**

> RFC：`docs/rfc/0006-core-config-provider.md`

### Phase A：CoreConfig + CoreConfigProvider

- [x] 定义 `CoreConfig` 结构（models/mcp/skills/trust_mode/context_limit）
- [x] 实现 `CoreConfigProvider`（ArcSwap + generation 原子计数）
- [x] 修改 `TiangongCore` 构造函数接收 `CoreConfigProvider`
- [x] 修改 `worker_loop` 使用 generation 检测 + snapshot 重建 engine
- [x] 移除 `build_engine()` 中的磁盘加载逻辑

### Phase B：各端适配

- [x] CLI：创建 CoreConfigProvider，从磁盘加载初始配置，并在 `/model` `/config set` `/mcp` `/skill` 变更后同步
- [x] GUI：TiangongApp 持有 CoreConfigProvider，模型/MCP/Skill/TrustMode 变更时同步 update
- [ ] Server/Connector：适配 CoreConfigProvider

### Phase C：验证

- [x] cargo clippy --workspace 通过
- [x] cargo nextest run --workspace 通过
- [ ] GUI 验证：切换模型/MCP/Skill 后下一轮对话生效
- [ ] CLI 验证：/model 切换后生效
