# T4 - handler 按 session_id 路由

## 目标
`browser_command_handler` 从持全局 `browser_state` 改为持 `Arc<BrowserSessionRegistry>`，每个 arm 从命令取 session_id，路由到对应 session 的 state 操作。

## 范围
- `crates/plugins/tiangong-plugin-browser/src/handler.rs`（command handler 全部 16 个 arm）
- `crates/plugins/tiangong-plugin-browser/src/lib.rs`（`init()` 里 spawn handler 时传 registry 而非 browser_state）

## 依赖
- 前置任务：T2（registry）、T3（command 带 session_id）
- 后续任务：T6（watcher observe 路由）
- 可并行任务：T5 可并行（switch_session 改造与 handler 路由不冲突）
- 阻塞说明：command 必须先带 session_id，handler 才能按它路由。

## 任务
- `browser_command_handler` 签名：`browser_state: Arc<Mutex<BrowserState>>` → `registry: Arc<BrowserSessionRegistry>`。
- 每个 arm 从命令解构出 `session_id`，用 `registry.session_state(&session_id)`（session_id 为空时回退 `active_state()`）获取 `BrowserManager { state }`。
- `init()` 的 spawn 改为传 `registry`（从 `BrowserPluginState.manager` 拿，或 state 直接持 registry）。
- helper 函数 `wait_for_content_change` / `compute_page_diff` / `merge_diff_and_events` 接收 session 的 manager。

## 不做
- 不改 switch_session（T5）。
- 不改轮询/data_directory（T5）。
- 不改 watcher（T6）。
- 不改 manager 方法签名（manager 已在 T2 支持 state 获取）。

## 验收
- handler 按 command 的 session_id 路由到正确 session 的 state。
- 空 session_id 回退 active session（兼容旧启动路径）。
- `cargo check -p tiangong-plugin-browser` 通过。

## 验证
- `cargo check -p tiangong-plugin-browser`
