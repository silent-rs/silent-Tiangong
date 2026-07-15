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
- [ ] 删除 `worker_loop_async`(及其依赖的 forwarder / turn_capture / send_final_stream_event 等,逻辑已迁移到 `run_turn`)
- [ ] 删除 `build_turn_context` 函数(已被 `TurnContextBuilder` 替代)
- [ ] 删除 `build_context_from_config` 函数(已被 `TurnContextBuilder` 替代)
- [ ] `deliver(Message)` 改为用 `TurnContextBuilder`(当前仍调 `build_turn_context`)
- [ ] `run_turn` 完善:`plugins` 引用正确传入(当前从 `ctx.tools` 取,不准确)
- [ ] `run_turn` 完善:插件钩子(`on_turn_started` / `on_turn_finished` / `on_cancel`)需要 `&mut session`
- [ ] `run_turn` 的 `turn_cmd_tx` 正确存入 `TURN_TASKS`(当前用 `std::mem::forget` 占位)
- [ ] `std::mem::forget(turn_cmd_tx)` 移除 — 应改为 `spawn_turn` 返回的 cmd_tx 存入 TURN_TASKS

### execute_turn 内部简化
- [ ] `execute_turn` 从 `self.session` 读取(当前仍接收 `session: &mut Session` 参数)
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
