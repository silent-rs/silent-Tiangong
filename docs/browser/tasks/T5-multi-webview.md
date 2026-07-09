# T5 - 多 webview 并发 + 切换不销毁

## 目标
切换 session 时不再销毁 webview，而是隐藏旧 session + 显示新 session；每个 session 独立 data_directory 隔离 cookie/storage；修复 on_page_load 历史归属 bug。

## 范围
- `crates/plugins/tiangong-plugin-browser/src/manager.rs`（switch_session / reset_runtime_state / 轮询 / create_webview_for_tab / on_page_load）

## 依赖
- 前置任务：T2（manager 持 registry）、T4（handler 路由，switch_session 命令经路由）
- 后续任务：T6（watcher observe 依赖 webview 存活）
- 可并行任务：可与 T4 并行的部分有限（switch_session 在 handler 链路里）
- 阻塞说明：registry + handler 路由必须先就绪，switch_session 才能正确切换 active。

## 任务
- `switch_session` 重写：不同 session 切换时，不再 `reset_runtime_state(true)` 销毁全部 webview。改为：
  - 隐藏旧 active session 的全部 webview（`set_position(-10000,-10000)`）。
  - `registry.set_active(new_session_id)`。
  - 显示新 session 的 active tab webview。
  - 停旧 session 轮询、启新 session 轮询。
- `reset_runtime_state` 拆分：
  - 新增 `hide_session_webviews(state)`：把该 session state 内所有 webview off-screen，不停轮询标志的进程级语义（轮询改为 per-session）。
  - `close_session(id)`：仅在 session 删除时真正关闭该 session 全部 webview + 清理 state。
- 轮询改为 per-session：`start_url_poll`/`start_event_poll` 对指定 session 的 active webview；切换 session 时停旧启新。轮询标志从全局单对改为 per-session（在 BrowserState 内，每个 session 自己的 poll_stop/event_poll_stop）。
- `create_webview_for_tab` 加 session_id 参数，data_directory = `~/.tiangong/browser-data/<session_id>`。
- on_page_load 历史归属 bug 修复：用实际加载的 `tab_id_for_closure` 记录 tab 历史，不用全局 `active_tab_id`。

## 不做
- 不做 OS 层多 webview 并发的性能调优（验证可行即可）。
- 不做 webview 数量上限。
- 不改 z-order（无 API，靠 off-screen）。

## 验收
- 切换 session 不销毁 webview，切回仍见原页面。
- 各 session cookie/storage 独立（不同 data_dir）。
- on_page_load 历史归属正确（多 session 并发加载不串）。
- `cargo check -p tiangong-plugin-browser` 通过。

## 验证
- `cargo check -p tiangong-plugin-browser`
- **手动验证（必须）**：macOS 真实环境——session A 开 google.com，切 B 开 example.com，切回 A 仍见 google.com。多 session 并发加载页面观察内存。
