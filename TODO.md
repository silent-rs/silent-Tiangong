# TODO - 天工当前开发任务

> 最后更新：2026-06-02
> 当前主线：Phase 21 — 内嵌浏览器面板（0.5.0）
> 参考：`PLAN.md`、Issue #95

---

## Phase 21-A：技术验证

- [x] 验证 Tauri 2 多 WebView：在主窗口内创建第二个 WebView 加载外部 URL
- [x] 验证 `initialization_script` 正确注入到外部 URL 页面
- [x] 验证 IPC 双向通信（eval_with_callback 替代 title IPC）
- [x] 验证 WebView 位置跟随前端容器动态调整
- [x] 验证 data_directory 配置实现 Cookie 持久化

## Phase 21-B：BrowserManager 核心

- [x] 新增 `src-tauri/src/browser.rs` — BrowserManager 模块
- [x] Bridge Script v0.5.0 — getFullText（textContent 提取）、click、type
- [x] Tauri Commands：`browser_open`、`browser_close`、`browser_set_position`、`browser_navigate`、`browser_eval`
- [x] Agent `web_fetch` 拦截 → 浏览器获取页面内容（eval_with_callback + on_page_load + Condvar）
- [x] 内容就绪检测（wait_for_content_ready：内容增长稳定策略）
- [x] 新增 `BrowserPageSnapshot`、`PageStatus`、`ObservePage` 命令类型
- [x] `on_page_load` 自动捕获页面快照到 `latest_snapshot`

## Phase 21-C：前端浏览器面板

- [x] `BrowserPanel` 组件（地址栏 + WebView 容器 + 关闭按钮）
- [x] 集成到主布局（右侧可折叠面板）
- [x] 监听 `browser:open` 事件自动显示面板
- [x] 容器位置同步（resize 时更新 WebView 位置）
- [x] 拖拽手柄调整面板宽度
- [x] StatusPanel 添加浏览器开关按钮

## Phase 21-D：Agent 交互增强（待开发）

- [ ] 注册 `web_browse` 工具 — Agent 主动获取当前浏览器页面快照
- [ ] `ObservePage` 命令接入 RuntimeEngine
- [ ] 页面内容变化注入对话链（tool 消息格式）
- [ ] 页面导航事件通知前端（`browser:page_loaded`）

## Phase 21-E：表单填写增强（待开发）

- [ ] Bridge Script 实现三层表单填写策略（native setter / keyboard / paste）
- [ ] 适配 Ant Design、Element Plus 等 UI 库的 Select/DatePicker 组件
- [ ] 自动检测页面框架类型（React/Vue/vanilla）
- [ ] MCP Tool 层实现策略自动选择和 fallback

## Phase 21-F：完善与优化（待开发）

- [ ] Cookie 持久化配置确认（data_directory 已设置，验证跨会话保持）
- [ ] 浏览器操作的权限审批机制（复用现有审批流程）
- [ ] 前进/后退/刷新导航控制
- [ ] 多标签页支持（可选）
- [ ] 用户批注模式（在页面上标注，Agent 理解标注内容）

---
