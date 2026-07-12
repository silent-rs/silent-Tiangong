# RFC 0016 - 浏览器 per-session 架构设计

关联需求：`docs/browser/05-per-session-architecture.md`

> 后续状态（2026-07-12）：双写过渡已结束，Core `Session.tabs` / `active_tab_id` 已废弃；浏览器和终端分别以插件 per-session store 为真相源，Desktop 只保存跨插件顺序和活跃项的 `{kind, id}` 引用。

## 完成标准

浏览器从「全局单例 + 切换时销毁重建」改为「每个 session 独立 browser state，多 webview 并发存活，切换不销毁」。每个 session 拥有独立的 webview/tab/历史/cookie-storage/恢复数据，浏览器插件自管 per-session 持久化。core 不感知浏览器 session 细节。

## 设计原则

1. **镜像 terminal 插件的 per-session 模型**。terminal 已用 `SessionPtyRegistry: HashMap<session_id, Arc<SessionTabs>>` 验证过这条路径，browser 沿用相同结构。
2. **最小化签名改动**。`BrowserManager` 的 35 个方法签名尽量不变，通过 `active_state()` / `session_state(id)` 内部获取 state；只有需要显式 session 路由的入口（agent fetcher/watcher、switch_session）才传 session_id。
3. **分层路由**。前端命令作用于 active session（符合现状）；agent 工具调用经 session_id 显式路由。
4. **不新增 Plugin trait 方法**。session_id 从 `on_session_ready(&self, session: &mut Session)` 的 `session.id` 获取。
5. **渐进式、每步可编译**。按任务 spec 逐步推进，每个任务独立提交、独立验证。

## 关键决策

### D1. 运行时对象归谁管理
浏览器运行时状态（webview/tab/轮询/事件）归**浏览器插件的 `BrowserSessionRegistry`** 管理，按 session_id 隔离。core 不持有任何浏览器运行时状态。

### D2. 数据归谁保存
- **浏览器 tab/URL/title 恢复数据**：迁到浏览器插件的 per-session store（`~/.tiangong/browser-sessions/<session_id>.json`），由插件持久化与恢复。本阶段保留 Core `Session.tabs` 的 browser tab 字段兼容（前端双写/双读过渡），后续单独废弃。
- **全局浏览历史**（跨 session 共享）：保留在 `~/.tiangong/browser-history.json`，进程级，不变。
- **zoom**：保留进程级 `~/.tiangong/browser-zoom.json`（zoom 是用户偏好，非 session 级）。

### D3. 前端是事实源还是展示协调层
**后端插件 store 是真相源**。前端 `TabsContainer` 从后端读取/回灌 tab，但持久化与恢复由后端负责。切换 session 时前端调用 `browser_switch_session`，后端负责切换可见性 + 返回该 session 的 tab 快照。

### D4. session_id 注入时机
经 `Plugin::on_session_ready(&self, session: &mut Session)` 注入。此时 session.id 已确定、workspace/trust_mode/feedback_tx 已注入、watcher 尚未真正开始 observe（懒启动）。`BrowserPlugin` override 该钩子，把 session.id 存入 fetcher 和 watcher。

### D5. 多 webview 可见性
无 z-order API，靠 off-screen 定位。切换 active session 时：隐藏旧 session 全部 webview（`set_position(-10000,-10000)`），显示新 session 的 active tab webview。非 active session 的 webview 保持存活但不可见、不轮询。

### D6. cookie/storage 隔离
每个 session 独立 `data_directory`：`~/.tiangong/browser-data/<session_id>`。Tauri `WebviewBuilder::data_directory()` 已支持 per-webview 设置。

### D7. 旧数据兼容
- Core `Session.tabs` 的 browser tab：保留，前端继续读写（过渡），后端 store 作为补充真相源。
- `global_history` / `zoom`：进程级不变，迁移不触及。
- 现有 `browser_switch_session` 的 `tabs_to_restore` 参数：保留兼容（前端可继续传），后端优先用自己的 store。

## 数据模型

### BrowserSessionRegistry（新增，进程级单例）
```
sessions: Mutex<HashMap<session_id, Arc<Mutex<BrowserState>>>>
active_session_id: Mutex<Option<String>>
```

### BrowserState（现有，语义从全局→单 session）
保留现有全部字段（webviews/page_loaded_signals/latest_snapshots/tabs/active_tab_id/轮询标志/pending_events/tab_histories 等）。移除 `active_session_id`（上移到 registry）。`global_history`/`zoom_factor` 可保留在 state 内（各 session 独立副本）或上移进程级——**决策：上移进程级**，避免 N 份副本。

### BrowserSessionStore（新增，per-session 持久化）
```
~/.tiangong/browser-sessions/<session_id>.json
{
  tabs: [{ id, url, title }],
  active_tab_id: Option<String>,
  last_url: Option<String>,
  last_title: Option<String>
}
```

## 后端设计

### session_registry.rs（新增）
`BrowserSessionRegistry`：`session_state(id)` 懒创建、`active_state()` 取当前、`set_active(id)` 切换、`destroy_session(id)` 销毁。

### manager.rs（改造）
- `BrowserManager` 从持单个 `Arc<Mutex<BrowserState>>` 改为持 `Arc<BrowserSessionRegistry>`。
- 现有方法经 `self.registry.active_state()` 获取 state（前端命令路径，行为不变）。
- 新增 `session_state(id)` 显式路由方法（agent 路径）。
- `switch_session` 重写：不销毁 webview，改为隐藏旧 session + 显示新 session。
- `reset_runtime_state` 拆为 `hide_session_webviews(id)`（隐藏）和 `close_session(id)`（销毁，仅 session 删除时）。
- 轮询线程改为对指定 session 的 active webview；切换时停旧启新。
- `create_webview_for_tab` 加 session_id 参数，用于 per-session data_directory。
- `on_page_load` 历史归属 bug 修复（用实际 tab_id 而非全局 active_tab_id）。

### types.rs（改造）
`BrowserCommand` 全部 16 个变体加 `session_id: String` 字段。

### handler.rs（改造）
`browser_command_handler` 从持全局 `browser_state` 改为持 `Arc<BrowserSessionRegistry>`，每个 arm 从命令取 session_id，`registry.session_state(id)` 获取 state 操作。

### page_fetcher.rs（改造）
`BrowserPageFetcher` 持 `session_id: RwLock<Option<String>>`（`on_session_ready` 延迟注入），每个发出的命令带 session_id。

### watcher.rs（改造）
`BrowserWatcher` 持 `session_id`，observe 命令带自己的 session_id；`run_loop` 检测 `feedback_tx.is_closed()` 时退出（防泄漏）。

### session_store.rs（新增）
`BrowserSessionStore`：load/save per-session 恢复数据。

## 前端设计

- `TabsContainer.tsx` 的 `syncBrowserRuntimeForTabs` 保留（兼容），但 tab 真相源转移到后端。
- `browser_switch_session` 调用不变（后端内部改为不销毁 webview）。
- 新增：session 创建时后端自动恢复该 session 的浏览器 tab（前端无需主动回灌，但回灌仍兼容）。

## AI / Agent 行为

- `web_fetch`/`web_form_*`/`web_click`/`web_query_dom`/`web_locate_element` 路由到调用方所属 session 的浏览器。
- watcher 只 observe 自己 session 的页面，注入自己 session 的 feedback。
- 不改变工具规格、工具名、权限分类。

## 兼容性策略

- Core `Session.tabs` browser tab 字段：保留读写（过渡）。
- `browser_switch_session` 的 `tabs_to_restore`：保留兼容。
- `global_history`/`zoom`：进程级不变。
- 旧的单 session 启动路径（无 active session 时回退首个注册 session）保留兼容。

## 验收清单

- [ ] session A 开 google.com，切 B 开 example.com，切回 A 仍见 google.com（webview 未销毁）。
- [ ] A/B 的 cookie/storage 独立（不同 data_dir）。
- [ ] Agent 在 A 调 web_fetch 不影响 B 的浏览器。
- [ ] watcher 只注入自己 session 的 feedback。
- [ ] 删除 session 关闭其 webview + 清理 store。
- [ ] 重启后各 session 恢复上次 tab/URL。
- [ ] `cargo check --workspace --tests` 零 warning/error。
- [ ] core/browser/terminal/app-state 测试全过。

## 任务拆分入口

见 `docs/browser/tasks/` 目录，共 8 个任务（T1-T8），依赖关系见 PROGRESS.md。
