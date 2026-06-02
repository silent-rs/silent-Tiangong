# TODO - 天工当前开发任务

> 最后更新：2026-06-03
> 当前主线：Phase 21 — 内嵌浏览器面板（0.5.0）
> 参考：`PLAN.md`、Issue #95

---

## 已完成

### Phase 21-A：技术验证

- [x] 验证 Tauri 2 多 WebView：在主窗口内创建第二个 WebView 加载外部 URL
- [x] 验证 `initialization_script` 正确注入到外部 URL 页面
- [x] 验证 IPC 双向通信（eval_with_callback 替代 title IPC）
- [x] 验证 WebView 位置跟随前端容器动态调整
- [x] 验证 data_directory 配置实现 Cookie 持久化

### Phase 21-B：BrowserManager 核心

- [x] 新增 `src-tauri/src/browser.rs` — BrowserManager 模块
- [x] Bridge Script v0.5.0 — getFullText（textContent 提取）、click、type
- [x] Tauri Commands：`browser_open`、`browser_close`、`browser_set_position`、`browser_navigate`、`browser_eval`
- [x] Agent `web_fetch` 拦截 → 浏览器获取页面内容（eval_with_callback + on_page_load + Condvar）
- [x] 内容就绪检测（wait_for_content_ready：内容增长稳定策略）
- [x] 新增 `BrowserPageSnapshot`、`PageStatus`、`ObservePage` 命令类型
- [x] `on_page_load` 自动捕获页面快照到 `latest_snapshot`

### Phase 21-C：前端浏览器面板

- [x] `BrowserPanel` 组件（地址栏 + WebView 容器 + 关闭按钮）
- [x] 集成到主布局（右侧可折叠面板）
- [x] 监听 `browser:open` 事件自动显示面板
- [x] 容器位置同步（resize 时更新 WebView 位置）
- [x] 拖拽手柄调整面板宽度
- [x] StatusPanel 添加浏览器开关按钮

### Phase 21-D：Agent 交互增强

- [x] 注册 `web_browse` 工具 — Agent 主动获取当前浏览器页面快照
- [x] `ObservePage` 命令接入 RuntimeEngine
- [x] 页面导航事件通知前端（`browser:page_loaded`）

---

## 当前优先：Phase 21-G — 浏览器能力插件化

> POC 已验证通过，现在将浏览器能力从应用代码中抽离为独立 Tauri Plugin。
> 详细方案：`docs/rfc/0014-browser-plugin-extraction.md`

### G-1：Core 层 Trait 抽象

> 前置步骤，无功能变化。为 plugin 化提供解耦接口。

- [ ] 新增 `tiangong-core/src/browser_trait.rs` — `PageFetcher` trait、`FetchResult`、`PageSnapshot`
- [ ] `RuntimeEngine` 新增 `page_fetcher: Option<Arc<dyn PageFetcher>>` 字段
- [ ] 新增 `set_page_fetcher()` 方法（通过 Command 传递，引擎重建时保留）
- [ ] `try_browser_fetch()` / `try_browser_observe()` 改为调用 trait object
- [ ] 新增 `register_tool_override(name, handler)` 机制，替代硬编码 `if call.name == "web_fetch"` 拦截
- [ ] 验证：`page_fetcher` 为 None 时回退到 HTTP `web_fetch`，CLI/Server 模式不受影响

### G-2：Plugin Crate 搭建

> 将 `src-tauri/src/browser.rs` 迁移为独立 crate。

- [ ] 创建 `tiangong-plugin-browser` crate
- [ ] 迁移 `BrowserManager` → `src/manager.rs`
- [ ] 迁移 JS Bridge Script → `src/bridge.rs`
- [ ] 迁移命令处理循环 → `src/handler.rs`
- [ ] 迁移 Tauri commands → `src/commands.rs`
- [ ] 实现 `PageFetcher` trait → `src/page_fetcher.rs`
- [ ] Plugin 入口 `src/lib.rs` — `plugin::Builder` 注册 commands 和 state
- [ ] 创建 `guest-js/` 前端 API 包（invoke 封装）
- [ ] 验证：Plugin 可独立编译，单元测试通过

### G-3：应用层切换

> 主应用切换到使用 plugin，移除内联 browser 代码。

- [ ] `src-tauri/Cargo.toml` 添加 `tiangong-plugin-browser` 依赖
- [ ] `main.rs` 使用 `.plugin(tiangong_plugin_browser::init())` 注册
- [ ] `setup` 中创建 `BrowserPageFetcher` 并注入到 core（`set_page_fetcher`）
- [ ] `app.rs` 移除 `browser`、`browser_cmd_tx`、`browser_cmd_rx` 字段和 `start_browser_handler()`
- [ ] `commands.rs` 移除 `browser_open/close/hide/set_position/navigate/eval/go_back/go_forward`
- [ ] 前端 `api/tauri.ts` 中 browser API 改为调用 plugin 前端包
- [ ] 验证：全功能回归测试（打开/关闭/导航/Agent web_fetch/Agent web_browse/窗口 resize）

### G-4：清理

> 删除旧代码，恢复 crate 职责边界。

- [ ] 删除 `src-tauri/src/browser.rs`
- [ ] 删除 `tiangong-types/src/browser.rs`，移除 `tokio` 依赖
- [ ] 删除 `tiangong-core` 中旧的 `browser_tx` channel 相关代码和 `SetBrowserChannel` Command
- [ ] 删除 `tiangong-core` 中 `Command::SetBrowserChannel` 变体
- [ ] 更新 `PLAN.md`、`TODO.md` 标记完成

---

## 后续：Phase 21-D/E/F — 功能增强

> 插件化完成后，在 plugin 架构上继续开发功能增强。

### Phase 21-D（剩余）：Agent 交互增强

- [ ] 页面内容变化注入对话链（tool 消息格式）

### Phase 21-E：表单填写增强

- [ ] Bridge Script 实现三层表单填写策略（native setter / keyboard / paste）
- [ ] 适配 Ant Design、Element Plus 等 UI 库的 Select/DatePicker 组件
- [ ] 自动检测页面框架类型（React/Vue/vanilla）
- [ ] MCP Tool 层实现策略自动选择和 fallback

### Phase 21-F：完善与优化

- [ ] Cookie 持久化配置确认（data_directory 已设置，验证跨会话保持）
- [ ] 浏览器操作的权限审批机制（复用现有审批流程）
- [ ] 前进/后退/刷新导航控制
- [ ] 多标签页支持（可选）
- [ ] 用户批注模式（在页面上标注，Agent 理解标注内容）

---
