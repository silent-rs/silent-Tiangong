# 06 - 浏览器会话切换

## 目标

让浏览器插件的 Tab 状态绑定到当前会话，切换会话时恢复对应浏览器 Tab 元数据。

## 范围

- `crates/plugins/tiangong-plugin-browser/src/manager.rs`
- `crates/plugins/tiangong-plugin-browser/src/commands.rs`
- `crates/plugins/tiangong-plugin-browser/src/lib.rs`

## 依赖

- 前置任务：01。
- 后续任务：07、11、12、13。
- 可并行任务：02、03、04、08。
- 阻塞说明：浏览器会话切换需要统一 Tab 元数据来恢复浏览器 Tab 列表和活跃 Tab。

## 任务

- `BrowserManager` 增加当前绑定会话 id。
- 新增 `browser_snapshot_tabs()`。
- 新增 `browser_switch_session(session_id, tabs_to_restore, active_tab_id)`。
- 切换会话时关闭旧 WebView。
- 清理旧页面快照、待消费事件和相关运行时状态。
- 恢复新会话的浏览器 Tab 元数据。
- 切换后发出 Tab 更新事件。

## 不做

- 不重写浏览器历史。
- 不持久化页面 JS 状态。
- 不改前端统一容器。

## 验收

- 会话 A 和会话 B 浏览器 Tab 元数据互不覆盖。
- 切换回会话 A 后，恢复 A 的浏览器 Tab 列表和活跃 Tab。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
