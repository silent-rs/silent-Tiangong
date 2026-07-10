# T3 - BrowserCommand 带 session_id + fetcher 注入

## 目标
让 `BrowserCommand` 全部变体携带 `session_id`，`BrowserPageFetcher` 持有 session_id 并在每个命令中带上；`BrowserPlugin` 经 `on_session_ready` 注入 session_id 到 fetcher/watcher。

## 范围
- `crates/plugins/tiangong-plugin-browser/src/types.rs`（`BrowserCommand` 16 个变体加字段）
- `crates/plugins/tiangong-plugin-browser/src/page_fetcher.rs`（`BrowserPageFetcher` 持 session_id，发命令时带）
- `crates/plugins/tiangong-plugin-browser/src/plugin.rs`（override `on_session_ready`）
- `crates/plugins/tiangong-plugin-browser/src/watcher.rs`（持 session_id，供 T6 用）

## 依赖
- 前置任务：T2（manager 持 registry，handler 需要它来按 session_id 路由——但本任务只改 command 定义和 fetcher，handler 改在 T4）
- 后续任务：T4（handler 消费 session_id 路由）
- 可并行任务：无
- 阻塞说明：command 必须先带 session_id，handler 才能按它路由。

## 任务
- `BrowserCommand` 全部 16 个变体加 `session_id: String` 字段。
- `BrowserPageFetcher` 加 `session_id: RwLock<Option<String>>` + `set_session_id(&str)`；每个 trait 方法构造命令时带上当前 session_id（未注入时带空串，handler 视为 active）。
- `BrowserPlugin` override `on_session_ready(&self, session: &mut Session)`：读 `session.id`，调 `fetcher.set_session_id` + `watcher.set_session_id`。
- `BrowserWatcher` 加 `session_id: RwLock<Option<String>>` + `set_session_id`（本任务只加字段，observe 路由在 T6）。
- `commands.rs` 里直接操作 manager 的 Tauri 命令：本任务暂用 active session（不传 session_id，行为不变）；只有 `browser_switch_session` 已有 session_id 参数。

## 不做
- 不改 handler 路由逻辑（T4）。
- 不改 switch_session 销毁行为（T5）。
- 不改 main.rs 事件监听（T6）。

## 验收
- `BrowserCommand` 全部变体带 session_id。
- `BrowserPageFetcher` 发出的命令带正确 session_id。
- `on_session_ready` 正确注入 session.id。
- `cargo check -p tiangong-plugin-browser` 通过（handler 此时可能用 `_session_id` 忽略，T4 再消费）。

## 验证
- `cargo check -p tiangong-plugin-browser`
