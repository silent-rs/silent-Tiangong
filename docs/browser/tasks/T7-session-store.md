# T7 - per-session 浏览器持久化与恢复

## 目标
浏览器插件自管 per-session 恢复数据（tab/URL/title），应用重启后按 session 恢复各自上次的浏览器页面。

## 范围
- `crates/plugins/tiangong-plugin-browser/src/session_store.rs`（新增）
- `crates/plugins/tiangong-plugin-browser/src/manager.rs`（页面变化时更新 store）
- `crates/plugins/tiangong-plugin-browser/src/plugin.rs`（on_session_ready 时恢复）

## 依赖
- 前置任务：T3（on_session_ready 注入 session_id）、T6（watcher 检测变化）
- 后续任务：T8（端到端验证含恢复）
- 可并行任务：无
- 阻塞说明：session_id 注入就绪后才能按 session 持久化/恢复。

## 任务
- 新建 `session_store.rs`：`BrowserSessionStore`，load/save `~/.tiangong/browser-sessions/<session_id>.json`。
  - 结构：`{ tabs: [{id,url,title}], active_tab_id, last_url, last_title }`。
- 页面/tab 变化时（switch_session / tab_new / tab_close / on_page_load URL 变更）调用 `store.save_session(id, state)`。
- `on_session_ready` 时 `store.load_session(id)`，若有恢复数据则恢复该 session 的 tab（前端 hydrate 兼容，后端 store 为真相源）。
- session 销毁时删除 store 文件。
- `lib.rs` 声明 `pub mod session_store;`。

## 不做
- 不废弃 Core `Session.tabs` browser tab 字段（保留兼容，后续单独清理）。
- 不持久化 cookie/cache（由 webview data_directory 自然管理）。
- 不持久化全局历史（已是进程级）。

## 验收
- 重启后各 session 恢复上次 tab/URL。
- 删除 session 清理 store 文件。
- `cargo check -p tiangong-plugin-browser` 通过。

## 验证
- `cargo check -p tiangong-plugin-browser`
- 手动：开页面 → 重启 → 恢复。
