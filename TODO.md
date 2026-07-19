# #241 上下文压缩闭环调整 TODO

## 已完成

- [x] 压缩阈值改为“5%输出预算 + 1%安全余量”的派生值，单轮复用同一计算器。
- [x] 压缩提示词声明实际输出预算，并优先输出当前任务状态。
- [x] 将 Provider 停止原因传回 Core，拒绝空摘要和被截断摘要。
- [x] 压缩任务使用 Session 快照，成功持久化后再提交真实会话。
- [x] 自动压缩和手动压缩支持 Cancel/Shutdown。
- [x] 当前任务续接只进入下一次模型请求，不写入 Session 或前端消息。
- [x] 压缩成功、失败和取消均发布完整界面反馈。
- [x] 压缩结果提交、用量和通知统一由 ContextCompressor 处理，执行循环只推进状态。
- [x] 压缩用量进入本轮最终用量，压缩后的当前 token 使用实际输出校准。
- [x] 补齐预算、截断、一次性续接、取消和失败原子性测试。
- [x] 通过 Rust 检查、相关测试和前端构建。

## 完成标准

- 200k、1M 和更大上下文使用同一比例算法，不存在固定模型长度分支。
- 压缩失败或取消不会推进摘要边界。
- 最终答复后的下一轮不包含旧任务续接。
- 所有检查通过后再更新本节任务状态。

# #238 Core 资源模型重构 TODO

## 已完成

### 核心资源模型
- [x] 共享 tokio runtime(`shared_runtime.rs`)替代 per-Core 线程+runtime
- [x] 每 turn 现建 engine/client(配置正交,不跨 turn 复用)
- [x] 转发屏障改 async 通知(`tokio::sync::Notify`),移除 `block_in_place`
- [x] 移除持久化回执(`PreparedMessageReceipt` / `SessionMetadataReceipt`)
- [x] 非 turn 命令归 app(`UpdateCwd` / `UpdateSessionMetadata` / `ReloadConfig` 移除)

### TurnContext 提取
- [x] 合并 `ReactEngine` + `RuntimeEngine` 为 `TurnContext`
- [x] 删除 5 个死字段(`models_config` / `core_config` / `lite_client` / `tool_spec_providers` / `runtime_env`)
- [x] `TurnContext` 持有 `session` 字段
- [x] `TurnContextBuilder` 替代 `build_turn_context` 函数

### 权限简化
- [x] 删除 `PermissionGate` / `PermissionPolicy` / `TrustModeHandle` / `PermissionLevel` / `PermissionDecision`
- [x] 权限改为二元判断:FullTrust 放行一切,否则统一走审批(审批在 turn 层完成)
- [x] 删除 `classify_tool` / `tool_permission_overrides` trait 方法 / `evaluate_tool_permission`
- [x] 删除 `PathRule` / `NetworkRule` / `check_path` / `check_network`(死配置)
- [x] 插件 `set_trust_mode` 参数 `TrustModeHandle` → `TrustMode`(Copy)
- [x] `Command::SetTrustMode` 即时生效(通过 cmd_rx select! 更新 ctx.trust_mode)

### Observer 注入
- [x] `Observer` 结构体(持有 `storage_root`)注入 `TurnContext`
- [x] 审计函数从全局函数改为 `Observer` 方法
- [x] 删除 `audit.rs` + `observe/audit.rs`,合并为 `observe/mod.rs`

### 全局变量清理
- [x] 删除全局 `STORAGE_ROOT`(`storage.rs`)
- [x] 删除审批持久化(`approval_store.rs`)— 审批改为 turn 内瞬态
- [x] `approval_store` 函数改为参数传入 → 随文件删除
- [x] `Session::new_isolated` 接收 `storage_root` 参数
- [x] `Session::try_persist_to_disk` 不再回退全局,要求预先 bind

### Turn task 模型(进行中)
- [x] `shared_runtime` 新增 turn task 管理(`spawn_turn` / `send_command` / `is_running` / GC)
- [x] `TURN_TASKS` 改为 `HashMap<session_id, (cmd_tx, JoinHandle)>`
- [x] `TiangongCore` 结构体重写:删除 `worker_task` / `command_delivery_lock`,新增 `plugins` / `external_tx` / `storage_root`
- [x] `deliver(Message)` → `spawn_turn`(构建 TurnContext + 注入用户消息 + 落盘)
- [x] `deliver(Cancel/Approval/SetTrustMode)` → `send_command`
- [x] `is_stopped` / `is_busy` → 查 `shared_runtime::is_running`
- [x] Desktop 将 Core 实例存在与 turn 运行状态分离，空闲 Core 仍可接收下一条用户消息
- [x] `into_session` → 从磁盘 load
- [x] `builder.rs` 重写:删除 `.session()`,新增 `.session_id()` / `.trust_mode()` / `.storage_root()`
- [x] `run_turn` 函数实现(替代 `worker_loop_async` 的 Message 分支)
- [x] `TurnContextBuilder` 创建(替代 `build_turn_context` 函数)

## 待完成

### Turn task 模型收尾
- [x] 删除 `worker_loop_async` 及不再使用的旧 worker 辅助逻辑，保留的轮次转发逻辑统一由 `run_turn` 持有
- [x] 删除 `build_turn_context` 函数，`TurnContext` 直接使用 `TypedBuilder`
- [x] 删除 `build_context_from_config` 函数
- [x] `deliver(Message)` 在 `spawn_turn` 前完成用户消息落盘并立即发布 `UserMessage`，再构建 `TurnContext`
- [x] Session 用户消息写入覆盖内容校验、同 ID 处理、失败回滚与完整落盘，删除 `accept_prepared_user_message_with_options`；遗留工具调用统一由上一轮取消收尾闭合
- [x] `run_turn` 从 `ctx.plugins` 调用插件生命周期钩子
- [x] `run_turn` 的插件钩子(`on_turn_started` / `on_turn_finished` / `on_cancel`)使用 `&mut session`
- [x] 删除 `run_turn` 结束时的插件工作区二次同步，插件工作区只在 turn 启动前设置
- [x] 删除 `run_turn` 的无效 `Session` 占位替换，以及 `react/message` 中仅转发底层方法的包装
- [x] `Session::close_unfinished_tool_calls_with_reason` 在补齐悬空工具消息后立即落盘；补齐落盘失败时删除悬空调用并重新落盘，成功后再由调用方发布实际存在的失败 `ToolResult`
- [x] 取消后不在 `run_turn` 收尾阶段刷新延迟工具注入，暂存数据保留到下一 turn 安全点
- [x] 整理 `run_turn` 收尾流程，在轮次锚点、插件生命周期、执行、消息修复、持久化与终态发布节点补充说明
- [x] `run_turn` 启动旁路计时器，每秒通过 `stream_tx` 发布当前运行秒数，并在提交最终耗时前停止
- [ ] App 对已有对话或执行记录的会话禁止修改工作区
- [x] 收敛单一终态合同：`execute_turn` 返回明确执行结果，最终持久化后只由 `run_turn` 发送一次 `Done` / `Error`，宿主按终态重载 Session，不再等待额外消息事件
- [x] `spawn_turn` 接收已构建的 `TurnContext` 与 Future 构建闭包，内部创建本轮 `cmd_tx / cmd_rx`
- [x] `spawn_turn` 在创建 Future 前遍历 `ctx.plugins` 调用 `set_feedback_tx`，并将 `cmd_tx` 存入 `TURN_TASKS`
- [x] 插件 `on_session_ready` 与提示段落注入调整到 feedback 绑定之后、turn task 启动之前
- [x] `deliver(Message)` 删除本轮命令通道创建与 `cmd_tx` 传参
- [x] 删除 `std::mem::forget(turn_cmd_tx)` 占位逻辑
- [x] 删除 `TiangongCore.session_ready_fired`，每轮 Session 加载完成并绑定 feedback 后、收集提示段落与执行 Agent Loop 前调用 `on_session_ready`
- [ ] `on_session_ready` 只负责基于本轮 Session 刷新插件状态；插件自行保证一次性后台初始化不重复，重点处理 Agent Team `Coordinator::initialize` 的幂等性

### execute_turn 内部简化
- [x] 将 `TurnContext::execute_turn` 从 Context 成员方法改为 `react/execute.rs` 的独立基础函数，并删除失去独立职责的 `engine` 模块
- [x] 将 `execute_turn` 迁入独立模块，按命令处理、模型请求、响应处理、工具执行与总结步骤拆分 Agent Loop
- [x] 将 `start_tool_call` 从 `TurnContext` 抽离到独立的 ReAct 工具执行模块
- [x] `execute_turn` 从 `ctx.session` 取得本轮 Session，不接收额外 Session 参数
- [x] 明确职责边界：`deliver(Message)` 完整构建本轮 Session，`run_turn` 只负责执行、收尾与持久化
- [x] 删除 `AcceptedUserMessage` 及 `run_turn` 的对应参数，统一从 `ctx.session` 使用本轮用户消息、消息 ID 与轮次起点
- [x] 删除 `execute_turn` 的 `initial_user_message` 参数，当前用户输入统一从 `ctx.session` 读取
- [x] `run_summary_phase` / `force_final_response` 同理改用 `self.session`
- [x] 合并 `execute_agent_loop` 与 `execute_turn`，由 `execute_turn` 直接编排并返回本轮结果
- [x] 将 `execute_react_phase` 平铺进 `execute_turn`，固定为外层 `react_loop`、内层 `execute_loop`，在各阶段等待点直接监听运行时命令；不使用包裹整轮的 `execute_future`，不再抽离阶段编排方法或增加阶段结果转换
- [x] 将 `execute_turn` 的内层 `execute_loop` 原样抽离为独立方法，保留外层 `react_loop` 的总结重入编排与既有取消、命令、用量语义
- [x] 修正运行时插件注入合同：接收后立即向 App 发布待处理快照，在安全边界完整写入 Session，取消前已接收的数据不得丢失，且晚于当前请求到达的结果必须由 Agent 在后续请求中消费
- [x] 将 `execute_turn` 收敛为唯一事件循环和唯一 `cmd_rx` 接收者：在同一个 `tokio::select!` 中处理命令、模型流与工具执行，保留工具阶段轮次上限、总结重入上限、取消传播、实时插件反馈和用量累计语义
- [x] 同一 LLM 回复中的工具调用使用 Tokio 并行执行；每项完成后立即向 App 反馈、写入并持久化 Session，全部工具结束后再继续 Agent Loop，取消时统一终止并闭合未完成调用
- [x] 执行链统一只传 `TurnContext`，通过 `ctx.session` 访问会话，删除占位 Session 与 `ctx + session` 双参数
- [x] 删除 `TurnUsageSink` 与 turn 绑定旁路；插件用量统一通过 `PluginFeedbackTx` 命令上报，并直接累计到 `execute_turn` 本轮用量
- [x] 删除基于关键词的后台工具意图过滤和 `user_input` 局部状态，所有工具统一交由主模型结合 Session 上下文选择
- [x] 删除未参与控制流的 `TurnPhase` 枚举和无效阶段赋值，阶段变化只通过 `StreamEvent::PhaseChanged` 发布
- [x] 删除 `PendingCommandEffect`、运行中消息追加链路及通用命令处理封装；主 `cmd_rx` 直接在 `execute_turn` 外层 `react_loop` 中展开，收到 `Cancel` / `Shutdown` 后立即关闭接收端，并通过逐层新建的 `oneshot` 通知内层 `execute_loop` 由内向外收尾退出
- [x] 补齐 `execute_turn` 的请求失败、运行时命令、审批、工具/总结取消、总结重入与强制收尾测试，并用覆盖率报告复核关键控制分支
- [x] 补充 `execute_turn` 关键执行节点注释，说明双层循环、命令处理、取消传播、工具执行、总结重入与结果出口，不改变执行逻辑
- [x] 收紧忙碌期控制边界：手动压缩与清空上下文仅在 Core 空闲时执行，`execute_turn` 只接收取消、信任模式切换、审批及内部反馈；信任模式立即更新运行态并随轮次统一落盘；手动压缩复用 `spawn_turn`，并与自动压缩统一使用滚动摘要流程
- [x] 为 `TiangongCore` 增加无参数的 `build_turn_context`，内部加载 Session；普通投递统一通过 `ctx.session` 写入用户消息
- [x] `reset_context` 和手动压缩结束时不重建系统提示，下一轮对话启动时再统一重建
- [x] 删除手动压缩开始前的重复落盘，并让手动与自动压缩统一使用 `maybe_update_context_summary`
- [x] 简化手动压缩：使用 `Session.context()` 取得安全消息并按轮弹出，`maybe_update_context_summary` 只接收待压缩消息，由压缩器内部推导持久化边界
- [x] 拆分压缩阈值判断与压缩执行收尾，手动压缩直接执行且不再伪造用量

### 调用方适配
- [ ] `app.rs ensure_core`:先 persist session 文件再创建 Core(不再传 session 给 builder)
- [ ] `app.rs create_core`:适配新 builder API(`.session_id()` / `.trust_mode()` / `.storage_root()`)
- [ ] `app.rs retire_core`:`shutdown_join` 简化(不再等 worker task)
- [ ] `child_runtime.rs`:适配新 builder API + `deliver_and_wait` 适配 turn task 模型
- [ ] `CLI repl.rs`:`into_session` 从磁盘 load(不再从 worker 取回)
- [ ] `embedded_server.rs`:适配新 builder API

### 后续独立 issue
- [x] #245 app-state 仅保留本次运行状态(session 真相源归磁盘,移除完整 Session 列表 / RuntimeEngine / save_core_session / app.json 持久化)
- [x] `TiangongState` 启动时加载完整配置并据此创建 `CoreManager`，Desktop / CLI / Server 共用该实例
- [x] 新对话只预留最终 Session ID 和输入缓存，首次向 Core 投递消息时才创建并保存 Session
- [x] 将默认信任模式、默认工作目录和自定义 Prompt 纳入配置，并兼容旧 `app.json`
- [x] 修复 Desktop 运行中卡顿与流式输出、从模型 `context_window` 派生上下文与压缩阈值、终端 Tab 恢复目录错误
- [x] Agent 运行时间只使用 `stream_tx` 返回的 `TurnElapsed`，移除前端本地计时
- [ ] app-state 的 `RuntimeEngine` 替换为轻量配置结构体
- [ ] `Command::Message` / `AgentInputKind::Message` 双层映射合并
