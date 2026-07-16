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
- [x] `into_session` → 从磁盘 load
- [x] `builder.rs` 重写:删除 `.session()`,新增 `.session_id()` / `.trust_mode()` / `.storage_root()`
- [x] `run_turn` 函数实现(替代 `worker_loop_async` 的 Message 分支)
- [x] `TurnContextBuilder` 创建(替代 `build_turn_context` 函数)

## 待完成

### Turn task 模型收尾
- [x] 删除 `worker_loop_async` 及不再使用的旧 worker 辅助逻辑，保留的轮次转发逻辑统一由 `run_turn` 持有
- [x] 删除 `build_turn_context` 函数，`TurnContext` 直接使用 `TypedBuilder`
- [x] 删除 `build_context_from_config` 函数
- [x] `deliver(Message)` 在 `spawn_turn` 前完成 `TurnContext` 构建和用户消息落盘
- [x] `run_turn` 从 `ctx.plugins` 调用插件生命周期钩子
- [x] `run_turn` 的插件钩子(`on_turn_started` / `on_turn_finished` / `on_cancel`)使用 `&mut session`
- [x] `spawn_turn` 接收已构建的 `TurnContext` 与 Future 构建闭包，内部创建本轮 `cmd_tx / cmd_rx`
- [x] `spawn_turn` 在创建 Future 前遍历 `ctx.plugins` 调用 `set_feedback_tx`，并将 `cmd_tx` 存入 `TURN_TASKS`
- [x] 插件 `on_session_ready` 与提示段落注入调整到 feedback 绑定之后、turn task 启动之前
- [x] `deliver(Message)` 删除本轮命令通道创建与 `cmd_tx` 传参
- [x] 删除 `std::mem::forget(turn_cmd_tx)` 占位逻辑
- [x] 删除 `TiangongCore.session_ready_fired`，每轮 Session 加载完成并绑定 feedback 后、收集提示段落与执行 Agent Loop 前调用 `on_session_ready`
- [ ] `on_session_ready` 只负责基于本轮 Session 刷新插件状态；插件自行保证一次性后台初始化不重复，重点处理 Agent Team `Coordinator::initialize` 的幂等性

### execute_turn 内部简化
- [x] 将 `TurnContext::execute_turn` 从 Context 成员方法改为 `react/turn.rs` 的独立基础函数，并删除失去独立职责的 `engine` 模块
- [x] `execute_turn` 从 `ctx.session` 取得本轮 Session，不接收额外 Session 参数
- [x] 明确职责边界：`deliver(Message)` 完整构建本轮 Session，`run_turn` 只负责执行、收尾与持久化
- [x] 删除 `AcceptedUserMessage` 及 `run_turn` 的对应参数，统一从 `ctx.session` 使用本轮用户消息、消息 ID 与轮次起点
- [x] 删除 `execute_turn` 的 `initial_user_message` 参数，当前用户输入统一从 `ctx.session` 读取
- [ ] `run_summary_phase` / `force_final_response` 同理改用 `self.session`
- [ ] `execute_turn_async` wrapper 函数简化或内联

### 调用方适配
- [ ] `app.rs ensure_core`:先 persist session 文件再创建 Core(不再传 session 给 builder)
- [ ] `app.rs create_core`:适配新 builder API(`.session_id()` / `.trust_mode()` / `.storage_root()`)
- [ ] `app.rs retire_core`:`shutdown_join` 简化(不再等 worker task)
- [ ] `child_runtime.rs`:适配新 builder API + `deliver_and_wait` 适配 turn task 模型
- [ ] `CLI repl.rs`:`into_session` 从磁盘 load(不再从 worker 取回)
- [ ] `embedded_server.rs`:适配新 builder API

### 后续独立 issue
- [ ] #245 app-state 职责收窄(session 真相源归磁盘,移除完整 Session 列表 / RuntimeEngine / save_core_session)
- [ ] app-state 的 `RuntimeEngine` 替换为轻量配置结构体
- [ ] `Command::Message` / `AgentInputKind::Message` 双层映射合并
