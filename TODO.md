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

### C. CLI 实时流式展示（待验收）
- [ ] repl.rs 轮询循环从阻塞等待改为边轮询边输出增量
- [ ] output.rs 新增流式输出函数（解释文本、工具摘要、增量输出）
- [ ] 追踪消息增量，实现系统消息和助手消息的实时展示

### D. 验证
- [ ] `cargo check --workspace` 通过
- [ ] `cargo clippy --workspace --all-targets --tests --benches -- -D warnings` 通过
- [ ] 前端 `yarn build` 通过

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
- [ ] 迁移 `runtime.rs` 的 `RunStatus` 使用 `UnifiedTaskStatus`
- [ ] 迁移 `tool/background_task.rs` 的 `TaskStatus` 使用 `UnifiedTaskStatus`
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
- [ ] 修改 `tool/background_task.rs`：任务完成时触发通知
- [ ] 修改 `turn_runner.rs`：支持接收后台任务完成事件，注入会话上下文
- [x] 验证：`cargo check --workspace` 通过

#### B2. 恢复与持久化增强（GAP-9）
- [x] 新建 `src/task/persistence.rs`：任务状态持久化
  - 写入：`~/.tiangong/tasks/{task_id}.json`
  - 读取：启动时扫描恢复
- [x] 新建 `src/task/recovery.rs`：启动恢复逻辑
  - Running/Backgrounded → 标记为 interrupted
  - WaitingApproval → 恢复审批界面
- [ ] 修改 `app_state/facade/lifecycle.rs`：启动时调用 recovery
- [x] 验证：`cargo check --workspace` 通过

### Phase 11-C：能力增强（中优先级）

#### C1. 上下文装配层增强（GAP-2）
- [x] 新建 `src/context/memory.rs`：用户偏好与长期记忆
  - 从 `~/.tiangong/memory/` 加载
  - 支持会话级 / 全局级
- [x] 新建 `src/context/retriever.rs`：检索接口（预留 trait）
- [ ] 修改 `src/context/assembler.rs`：装配流程增加记忆注入步骤
- [x] 修改 `src/context/mod.rs`：导出新模块
- [x] 验证：`cargo check --workspace` 通过

#### C2. 多代理 Worker 隔离增强（GAP-4）
- [x] 修改 `src/coordinator/types.rs`：WorkerBudget 增加 max_tool_calls，WorkerContext 增加 is_tool_allowed()
- [ ] 修改 `src/coordinator/worker.rs`：执行时限制工具集和预算
- [ ] 修改 `src/coordinator/task_coordinator.rs`：创建 Worker 时注入隔离配置
- [x] 验证：`cargo check --workspace` 通过

#### C3. 权限细粒度控制（GAP-7）
- [x] 修改 `src/permission.rs`：扩展 PermissionPolicy
  - 新增 `PathRule`：路径级允许/拒绝规则
  - 新增 `NetworkRule`：网络目标白名单
- [x] 新增 `PermissionGate::check_path()` 和 `check_network()` 方法
- [ ] 修改 `src/observe/audit.rs`：审计记录增加参数摘要
- [x] 验证：`cargo check --workspace` 通过

#### C4. 观测与成本治理闭环（GAP-10）
- [x] 修改 `src/observe/cost.rs`：新增 RequestCost / TaskCost / SessionCost 三层
- [x] 新建 `src/observe/collector.rs`：统一采集入口 ObserveCollector
- [ ] 修改 `src/observe/metrics.rs`：集成到 TurnRunner 自动采集
- [x] 验证：`cargo check --workspace` 通过

### Phase 11-D：远程能力（低优先级）

#### D1. 远程接入角色模型（GAP-8）
- [x] 新建 `tiangong-gateway/src/role.rs`：RemoteRole 枚举（Controller/Approver/Observer）
- [x] 修改 `tiangong-gateway/src/message.rs`：IncomingMessage 增加 sender_role
- [ ] 修改 `tiangong-gateway/src/router.rs`：根据角色限制操作
- [x] 验证：`cargo check --workspace` 通过

### Phase 11-E：最终验证
- [ ] `cargo fmt -- --check` 通过
- [ ] `cargo check --workspace` 通过
- [ ] `cargo clippy --workspace --all-targets --tests --benches -- -D warnings` 通过
- [ ] `cargo nextest run --workspace --no-tests pass` 通过

---

## Phase 12：事件驱动循环运行时 — **当前阶段**

> RFC：`docs/rfc/0005-event-loop-runtime.md`

### Phase 12-A：EventLoopRunner 核心 + 挂起/恢复

- [ ] 新建 `src/event_loop/mod.rs` 模块入口
- [ ] 新建 `src/event_loop/types.rs`：LoopEvent / LoopPhase / LoopState 类型定义
- [ ] 新建 `src/event_loop/runner.rs`：EventLoopRunner 核心循环
  - 收集事件 → 注入上下文 → 组织上下文 → LLM 调用 → 判断满足/工具执行
  - 无事件时挂起（保存 LoopState，释放线程）
- [ ] 新建 `src/event_loop/context.rs`：事件到上下文的转换逻辑
- [ ] `lib.rs` 注册 `event_loop` 模块
- [ ] 验证：`cargo check --workspace` 通过

### Phase 12-B：LoopHost trait + ActiveLoops 管理器

- [ ] 新建 `src/event_loop/host.rs`：LoopHost trait（send_event / poll_output / shutdown_all）
- [ ] 新建 `src/event_loop/active_loops.rs`：MultiLoopHost（GUI/Server 多会话管理）
  - running / suspended 状态管理
  - send_event 自动唤起挂起的 loop
- [ ] 新建 `src/event_loop/cli_host.rs`：CliLoopHost（CLI 单会话）
- [ ] 验证：`cargo check --workspace` 通过

### Phase 12-C：生命周期管理与持久化

- [ ] 新建 `src/event_loop/persistence.rs`：PersistedLoopState 读写
- [ ] 实现 shutdown_all()：Running→中断保存、Suspended→写盘
- [ ] 实现启动恢复：扫描 ~/.tiangong/loops/ 加载到 suspended
- [ ] 实现 cleanup_inactive()：超时 loop 写盘移除
- [ ] 验证：`cargo check --workspace` 通过

### Phase 12-D：集成与清理

- [ ] 修改 `app_state/services/turn/`：start_turn 改为 send_event
- [ ] 修改 `app_state/facade/sessions/turn_control.rs`：poll_pending_turn 改为 poll_active_loop
- [ ] 修改 `tiangong-cli/src/repl.rs`：使用 CliLoopHost
- [ ] 删除 TurnRunner / QueryClassifier / ControlSignal 等旧代码
- [ ] 验证：完整检查链通过
