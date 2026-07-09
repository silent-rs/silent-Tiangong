# T6 - watcher 与事件路由 session-aware

## 目标
watcher 持 session_id，observe 命令路由到自己 session 的页面；main.rs 的事件监听按 session 路由；watcher 在 feedback channel 关闭后退出防泄漏。

## 范围
- `crates/plugins/tiangong-plugin-browser/src/watcher.rs`
- `src-tauri/src/main.rs`（browser:page_loaded / browser:events 事件监听器）

## 依赖
- 前置任务：T3（watcher 持 session_id 字段）、T4（handler 路由，observe 命令经路由）、T5（多 webview 存活，observe 才有意义）
- 后续任务：T7（持久化，watcher 检测变化时更新 store）
- 可并行任务：无
- 阻塞说明：webview 必须先能多 session 并发存活（T5），watcher observe 自己 session 的页面才有意义。

## 任务
- `BrowserWatcher` 的 `observe_page` 调用带上自己的 session_id（fetcher 已支持，T3）。
- `run_loop` 检测 `feedback_tx.is_closed()` 时 `break` 退出（防 task 泄漏）。
- `main.rs` 的 `browser:page_loaded` / `browser:events` 监听器：从事件 payload 取 session_id（webview 事件需携带来源 session），路由到对应 Core 的 ToolInjection（而非全局 `session_id: None`）。
  - webview 的 `on_page_load` / 轮询 emit 事件时需带 session_id（T5 的 create_webview_for_tab 已知 session_id）。

## 不做
- 不改 watcher 的节流/变化检测逻辑（已就绪）。
- 不改 feedback 通道机制。

## 验收
- watcher 只 observe 自己 session 的页面，只注入自己 session 的 feedback。
- feedback channel 关闭后 watcher task 退出。
- main.rs 事件按 session 路由，不串台。
- `cargo check --workspace` 通过。

## 验证
- `cargo check --workspace`
- 手动（可选）：Agent 在 A 观察，B 不受影响。
