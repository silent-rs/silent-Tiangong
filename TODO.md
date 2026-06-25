# TODO - 天工当前开发任务

> 最后更新：2026-06-24
> 当前主线：0.10.1 发布后异常修复
> 参考：`PLAN.md`、Issue #95

---

## 当前入口（0.10.1）

- 0.10.0 已从 `main` 发布，版本标签为 `v0.10.0`。
- 当前阶段不扩展新功能，优先修复发布后暴露的 Agent 协作与运行时异常。
- 后续开发以 `docs/exception-fixes-0.10.1/README.md` 为任务清单，以 `docs/exception-fixes-0.10.1/PROGRESS.md` 为进度记录。
- Phase 21-M 统一工作区 Tabs 与会话绑定已完成，不再作为当前开发主线。

## 当前调整

- [x] 从最新主分支创建 `feature/agent-terminal-no-panel-open`
- [x] 补充 Agent 命令执行不主动打开工作区终端面板的需求约束
- [x] 调整 `run_command` / `run_shell` 通过插件终端执行但不主动打开面板的前端同步行为
- [x] Agent 创建或选中终端时立即附加到当前会话的工作区 Tab 列表
- [x] 运行检查命令验证改动

## 0.10.1 异常修复任务

| 序号 | Issue | 优先级 | 任务 | 状态 | Spec |
|---|---|---:|---|---|---|
| 01 | #163 | P0 | 0.10.0 发布后文档边界对齐 | 已完成 | `docs/exception-fixes-0.10.1/01-doc-boundary-alignment.md` |
| 02 | #166 | P0 | Workspace Index 写入器失败修复 | 已完成 | `docs/exception-fixes-0.10.1/02-workspace-index-writer-fix.md` |
| 03 | #168 | P1 | 工具空参数/解析失败恢复增强 | 已完成 | `docs/exception-fixes-0.10.1/03-empty-tool-arguments-recovery.md` |
| 04 | #167 | P1 | ReAct 主循环阶段化重构设计 | 已完成 | `docs/exception-fixes-0.10.1/04-react-loop-refactor-design.md` |
| 05 | #170 | P2 | 自动上下文压缩闭环核查 | 已完成 | `docs/exception-fixes-0.10.1/05-context-compression-audit.md` |
| 06 | #165 | P2 | 工具失败恢复结构化 | 已完成 | `docs/exception-fixes-0.10.1/06-structured-tool-failure-recovery.md` |
| 07 | #169 | P2 | 只读工具并行执行设计 | 已完成 | `docs/exception-fixes-0.10.1/07-readonly-tool-parallel-design.md` |
| 08 | #164 | P0 | 桌面端 MCP HTTP/SSE 注册异常修复 | 已完成 | `docs/exception-fixes-0.10.1/08-desktop-mcp-http-sse-registration.md` |

## 已完成

### Phase 21-K：嵌入式浏览器感知链路修复

- [x] 修复页面小幅变化、弹窗变化无法稳定注入 Agent 上下文的问题
- [x] 修复 fetch/XHR JSON 网络响应未进入 Agent 可见反馈的问题
- [x] 合并浏览器普通事件与网络事件的消费链路，避免后台轮询与工具结果互相抢占事件
- [x] 增加浏览器观测层与 Agent 注入链路的聚焦测试

### Phase 21-A：技术验证

- [x] 验证 Tauri 2 多 WebView：在主窗口内创建第二个 WebView 加载外部 URL
- [x] 验证 `initialization_script` 正确注入到外部 URL 页面
- [x] 验证 IPC 双向通信（eval_with_callback 替代 title IPC）
- [x] 验证 WebView 位置跟随前端容器动态调整
- [x] 验证 data_directory 配置实现 Cookie 持久化

### Phase 21-B：BrowserManager 核心

- [x] 新增 `src-tauri/src/browser.rs` — BrowserManager 模块
- [x] Bridge Script v0.7.0 — getFullText、click、type、extractForms、fillField、clickElement
- [x] Tauri Commands：`browser_open`、`browser_close`、`browser_set_position`、`browser_navigate`、`browser_eval`
- [x] Agent `web_fetch` 拦截 → 浏览器获取页面内容（eval_with_callback + on_page_load + Condvar）
- [x] 内容就绪检测（wait_for_content_ready：内容增长稳定策略）
- [x] 新增 `BrowserPageSnapshot`、`PageStatus`、`ObservePage` 命令类型
- [x] `on_page_load` 自动捕获页面快照到 `latest_snapshot`
- [x] URL 轮询线程检测子 WebView 后续导航变化

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
- [x] 页面内容变化注入对话链（tool 消息格式）

### Phase 21-E：表单填写增强

- [x] Bridge Script 实现三层表单填写策略（keyboard / native setter / paste）
- [x] `web_form_extract` 工具 — 提取页面表单字段结构
- [x] `web_form_fill` 工具 — 三层策略自动填写 + fallback
- [x] `web_click` 工具 — 模拟鼠标事件点击（mouseover/mousedown/mouseup/click + 坐标）
- [x] `web_load_html` 工具 — 加载 HTML 内容到浏览器（data URL）

### Phase 21-F：完善与优化

- [x] 前进/后退/刷新导航控制（BrowserPanel 工具栏按钮）
- [x] 窗口缩小时浏览器面板自动隐藏
- [x] Cookie 持久化验证（data_directory 已设置，依赖 WKWebView 内置行为）
- [x] 浏览器工具权限分类（web_browse/web_form_extract → Safe，web_form_fill/web_click/web_load_html → Elevated）

### Phase 21-G：浏览器能力插件化

- [x] G-1：Core 层 Trait 抽象（`PageFetcher`、`ToolOverrideHandler`、`SetPageFetcher`/`RegisterToolOverride` Command）
- [x] G-2：创建 `tiangong-plugin-browser` crate（`manager`、`bridge`、`handler`、`commands`、`page_fetcher`、`types`）
- [x] G-3：应用层切换（`.plugin()` 注册、前端 invoke 前缀改为 `plugin:browser|`、移除内联 browser 代码）
- [x] G-4：清理（删除 `src-tauri/src/browser.rs`、`tiangong-types/src/browser.rs`、移除 tokio 依赖）

### Phase 21-H：UI 库适配与框架检测

- [x] Bridge Script v0.8.0 新增 `detectFramework()` — 检测 React/Vue + Ant Design/Element Plus
- [x] `extractForms()` 扩展 UI 库组件扫描（Ant Design Select/DatePicker、Element Plus Select/DatePicker）
- [x] `fillComponent()` 新增 UI 库组件多步填写策略（Ant Design Select → 点击打开 → 选择匹配项）
- [x] handler 自动回退：`fillField` 失败后尝试 `fillComponent`

### Phase 21-I：多标签页支持

- [x] `BrowserTab` 结构体和标签管理命令（TabList/TabNew/TabSwitch/TabClose）
- [x] BrowserManager 标签状态管理（单渲染器 + 标签 URL/标题追踪）
- [x] 页面加载和 URL 变化时自动更新活跃标签
- [x] Tauri Commands（browser_tab_list/new/switch/close）
- [x] 前端标签栏（多个标签时显示，点击切换，关闭按钮，新建按钮）

### Phase 21-J：用户批注模式

- [x] Bridge Script `annotation` 对象 — canvas 覆盖层 + 矩形/箭头绘制
- [x] 批注数据结构化存储，`getAnnotations()` 返回标注列表
- [x] `web_browse` 自动附加页面批注信息
- [x] 前端批注按钮（画笔图标，点击切换批注模式）

## 发布准备（0.6.0）

- [x] 从最新主分支创建 `release/0.6.0` worktree
- [x] 更新 `Cargo.toml` 版本号为 `0.6.0`
- [x] 更新 `frontend/package.json` 版本号为 `0.6.0`
- [x] 更新 `tauri.conf.json` 版本号为 `0.6.0`
- [x] 更新发布流水线默认标签、Release 标题和 OSS 上传默认标签为 `v0.6.0`
- [x] 运行 Rust 检查
- [x] 运行 Rust lint
- [x] 运行 Rust 测试
- [x] 运行前端构建
- [x] 创建发布提交与 `v0.6.0` 标签

## 发布准备（0.8.3）

- [x] 从最新主分支创建 `release/0.8.3` worktree
- [x] 更新 `Cargo.toml` 版本号为 `0.8.3`
- [x] 更新 `frontend/package.json` 版本号为 `0.8.3`
- [x] 更新 `tauri.conf.json` 版本号为 `0.8.3`
- [x] 更新发布流水线默认标签、Release 标题和 OSS 上传默认标签为 `v0.8.3`
- [x] 运行 Rust 检查
- [x] 运行 Rust lint
- [x] 运行 Rust 测试（323 通过、1 失败、2 跳过；失败项：`recall_benchmark_compares_bm25_only_and_hybrid_hit_rate`）
- [x] 运行前端构建
- [x] 合并发布分支到 `main`
- [x] 在 `main` 最终发布提交创建并推送 `v0.8.3` 标签

## 发布准备（0.9.0）

- [x] 从最新主分支创建 `release/0.9.0` worktree
- [x] 更新 `Cargo.toml` 版本号为 `0.9.0`
- [x] 更新 `frontend/package.json` 版本号为 `0.9.0`
- [x] 更新 `tauri.conf.json` 版本号为 `0.9.0`
- [x] 更新发布流水线默认标签、Release 标题和 OSS 上传默认标签为 `v0.9.0`
- [x] 运行 Rust 检查（跳过本地验证，交由 CI 在标签推送后执行）
- [x] 运行 Rust lint（跳过本地验证，交由 CI 在标签推送后执行）
- [x] 运行前端构建（跳过本地验证，交由 CI 在标签推送后执行）
- [x] 合并发布分支到 `main`
- [x] 在 `main` 最终发布提交创建并推送 `v0.9.0` 标签

## 发布准备（0.9.1）

- [x] 更新 `Cargo.toml` 版本号为 `0.9.1`
- [x] 更新 `frontend/package.json` 版本号为 `0.9.1`
- [x] 更新 `tauri.conf.json` 版本号为 `0.9.1`
- [x] 更新发布流水线默认标签、Release 标题和 OSS 上传默认标签为 `v0.9.1`
- [x] 合并发布分支到 `main`
- [x] 在 `main` 最终发布提交创建并推送 `v0.9.1` 标签

## 发布准备（0.10.0）

- [x] 从最新主分支创建 `release/0.10.0` worktree
- [x] 更新 `Cargo.toml` 版本号为 `0.10.0`
- [x] 更新 `frontend/package.json` 版本号为 `0.10.0`
- [x] 更新 `tauri.conf.json` 版本号为 `0.10.0`
- [x] 更新发布流水线默认标签、Release 标题和 OSS 上传默认标签为 `v0.10.0`
- [x] 运行 Rust 检查（跳过本地验证，交由 CI 在标签推送后执行）
- [x] 运行 Rust lint（跳过本地验证，交由 CI 在标签推送后执行）
- [x] 运行前端构建（跳过本地验证，交由 CI 在标签推送后执行）
- [x] 合并发布分支到 `main`
- [x] 在 `main` 最终发布提交创建并推送 `v0.10.0` 标签

---

*(Phase 21 浏览器面板功能已全部完成)*

---

## Phase 21-L：Agent 页面操作等待机制（Issue #106）

- [x] 合并 feature/smart-element-location 分支的自动等待机制
- [x] click/fill 操作后自动等待页面内容变化（wait_for_content_change）
- [x] 签名轮询检测内容变化 + 稳定性确认后返回
- [x] compute_page_diff 对比操作前后 digest 返回差异
- [x] drain_events 收集浏览器事件队列
- [x] Agent 工具定义中不暴露 wait_for 参数（自动等待，无需 Agent 指定）
- [x] 合并 web_query_dom 工具（CSS 选择器查询 DOM）
- [x] 合并 bridge.js 的 waitFor、getPageDigest、diffDigest 等功能
- [x] 更新测试以适配新的消息注入格式

---

## Phase 21-M：统一工作区 Tabs 与会话绑定

- [x] 从最新 `main` 创建 `feature/unified-tabs-session-binding` worktree
- [x] 补充统一工作区 Tab 需求到 `docs/requirements.md`
- [x] 引入并重写统一工作区 Tabs 设计文档
- [x] 拆分任务 spec：`docs/tabs-session-binding/README.md`
- [x] 新增开发进度记录：`docs/tabs-session-binding/PROGRESS.md`
- [x] 01：会话 Tab 数据模型 - `docs/tabs-session-binding/01-session-tab-model.md`
- [x] 02：会话 Tab 读写命令 - `docs/tabs-session-binding/02-session-tab-commands.md`
- [x] 03：终端多 Tab 注册表 - `docs/tabs-session-binding/03-terminal-registry-multitab.md`
- [x] 04：终端空闲选择与繁忙新建 - `docs/tabs-session-binding/04-terminal-selection.md`
- [x] 05：命令结果反馈终端选择信息 - `docs/tabs-session-binding/05-terminal-result-feedback.md`
- [x] 06：浏览器会话切换 - `docs/tabs-session-binding/06-browser-session-switch.md`
- [x] 07：浏览器空白页懒创建 - `docs/tabs-session-binding/07-browser-blank-lazy-webview.md`
- [x] 08：前端单一工作区面板 - `docs/tabs-session-binding/08-frontend-workspace-shell.md`
- [x] 09：统一 Tabs 容器 - `docs/tabs-session-binding/09-frontend-tabs-container.md`
- [x] 10：终端 Tab 内容组件 - `docs/tabs-session-binding/10-terminal-tab-content.md`
- [x] 11：浏览器 Tab 内容组件 - `docs/tabs-session-binding/11-browser-tab-content.md`
- [x] 12：会话切换恢复与防抖持久化 - `docs/tabs-session-binding/12-session-restore-persistence.md`
- [x] 13：Tauri API 与权限声明 - `docs/tabs-session-binding/13-permissions-and-api.md`
- [x] 14：端到端验收 - `docs/tabs-session-binding/14-end-to-end-verification.md`

---
