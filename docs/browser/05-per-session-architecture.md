# 05 - 浏览器 per-session 架构需求

## 用户真实想解决什么问题

当前内嵌浏览器是**进程级全局单例**：无论打开多少个对话（session），它们共享同一个浏览器实例。切换对话时，旧对话的 webview 被全部销毁，新对话的 tab 被塞进全局浏览器重建。这导致：

- 对话 A 在浏览的页面，切到对话 B 后就没了，切回来要重新加载。
- 多个对话无法各自保持独立的登录态、cookie、浏览上下文。
- 浏览器状态（当前 URL、tab 列表、页面恢复信息）没有独立的 per-session 归属，全局污染风险（已在 trait 迁移 review 中暴露：watcher 跨 session 广播）。

用户希望：**每个对话拥有自己独立的浏览器实例/状态，切换对话不丢失各自的页面，浏览器插件自管对话级数据（缓存、URL、页面恢复）。**

## 用户最终能感知到的变化

1. 在对话 A 打开 google.com，切到对话 B 打开 example.com，再切回 A，A 仍显示 google.com（webview 未销毁，无需重新加载）。
2. 对话 A 和 B 各自的登录态/cookie 独立（不同站点登录互不影响）。
3. 关闭并重开应用后，每个对话恢复各自上次的浏览器页面。
4. Agent 在对话 A 里用 web_fetch 打开的页面，不会污染对话 B 的浏览器上下文。

## 必须满足的行为

- 每个 session 拥有独立的 webview 集合、tab 列表、浏览历史、cookie/storage。
- 切换 session 时，旧 session 的 webview 隐藏（不销毁），新 session 的 webview 显示。
- 同一时刻只有一个 session 的浏览器可见（active session），但后台 session 的 webview 保持存活。
- Agent 工具调用（web_fetch 等）路由到调用方所属 session 的浏览器，不操作其他 session。
- 浏览器页面恢复数据（URL/tab/title）由浏览器插件按 session 持久化，应用重启后恢复。
- 浏览器自动观察（watcher）只观察自己 session 的页面，只注入自己 session 的 feedback 通道。

## 明确不做

- 不做多窗口（多个独立 OS 窗口）。所有 session 的 webview 仍在单一 main 窗口内，作为 child webview。
- 不做 session 间浏览器状态共享/同步（各自独立）。
- 不改变工具名、工具 schema、StreamEvent 变体、Tauri command 对前端的接口契约（command 内部路由改，签名兼容）。
- 不在本阶段废弃 Core `Session.tabs` 里的 browser tab 字段（保留兼容，后续单独清理）。
- 不做浏览器实例数量硬上限（依赖 OS 资源自然约束，后续视情况加）。

## 异常与边界

- **webview 并发的 OS 层约束**：WKWebView/WebView2 共享或独立 data_dir 时的存储锁竞争，需在真实环境验证。每个 session 独立 data_dir 隔离 cookie/storage。
- **轮询线程倍增**：每个 active session 一对轮询线程（url poll + event poll）。非 active session 不轮询（静止），避免 N session = N 对线程。
- **z-order/可见性**：Tauri child webview 无显式 z-order API，靠 off-screen 定位（`set_position(-10000,-10000)`）隐藏。切换时需显式隐藏所有非 active session 的 webview。
- **session 销毁**：删除对话时关闭该 session 的全部 webview、清理 state、删除持久化文件。
- **应用重启恢复**：浏览器运行时状态（webview）不跨重启存活；tab 元数据（URL/title）持久化，重启后按 session 恢复。

## 与已有系统的关系

- **terminal 插件**已是 per-session 模型（`SessionPtyRegistry: sessions: HashMap<session_id, Arc<SessionTabs>>`），是本设计的直接参考模板。
- **PageFetcher/TerminalProvider trait 迁移**（本分支已完成）：browser/terminal 能力已内化进插件，core 不感知。本需求在此基础上让 browser 插件内部状态也 per-session 化。
- **`on_session_ready` 生命周期钩子**：session.id 在此时已确定，是注入 session_id 到 browser 插件的正确时机（register 时 session 可能未完全就绪）。
- **前端 `TabsContainer.tsx`**：当前通过 `browserSwitchSession(sessionId, tabsToRestore, activeTabId)` 驱动浏览器切换。后端 per-session 化后，tab 真相源转移到后端插件 store，前端 hydrate 逻辑需适配（但仍兼容现有回灌路径）。
