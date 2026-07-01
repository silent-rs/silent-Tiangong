# 浏览器页面缩放

> 状态：待实现
> 关联：`crates/plugins/tiangong-plugin-browser`、`frontend/src/components/BrowserPanel.tsx`、Issue #110

---

## 1. 背景

浏览器面板基于 Tauri 2 的 `Webview<Wry>`，每个标签页对应一个独立 webview。当前未暴露任何缩放能力，部分网页在小窗口下内容过小或布局错乱，用户被迫修改系统缩放来适配，影响可用性。

Issue #110 要求：

- 通过快捷键（`Cmd/Ctrl +/-`）或工具栏按钮调整缩放比例
- 缩放比例持久化，重启后恢复
- 提供"重置缩放"恢复 100%
- 缩放后页面截图、内容提取、批注框选等能力正常工作

## 2. 技术选型

调用 Tauri 2 `Webview::set_zoom(scale: f64)` / `Webview::zoom()`。

- 该 API 由 Tauri 统一封装，底层映射到 WKWebView（macOS）、WebView2（Windows）、WebKitGTK（Linux）的原生缩放能力
- 等比缩放整个 webview 渲染结果，**不影响 DOM 逻辑坐标**（`getBoundingClientRect`、`innerText` 等仍返回 layout 值）
- webview 内部的批注 canvas（`crates/plugins/tiangong-plugin-browser/js/bridge.js` `_ensureCanvas`）作为 DOM 元素，会随 webview 一起等比缩放，已绘制的批注视觉自动跟随

不采用 CSS `transform: scale()` 方案的原因：会破坏页面布局、干扰弹窗与覆盖层定位、影响 `position:fixed` 元素，且需要侵入 bridge.js。

## 3. API 设计

### Tauri 命令（`crates/plugins/tiangong-plugin-browser/src/commands.rs`）

| 命令 | 入参 | 返回 | 说明 |
|------|------|------|------|
| `plugin:browser\|browser_set_zoom` | `scale: f64` | `Result<f64, String>` | 设置缩放，clamp 到 `[0.25, 5.0]`，对当前所有 webview 同步生效并持久化；返回实际生效值 |
| `plugin:browser\|browser_get_zoom` | — | `Result<f64, String>` | 读取当前缩放（来自 `BrowserState.zoom_factor`） |
| `plugin:browser\|browser_reset_zoom` | — | `Result<f64, String>` | 重置为 1.0 |

### 缩放范围与步长

- 范围：`[0.25, 5.0]`，由后端统一 clamp，前端调用后以返回值为准
- 步长：`0.1`（10%）
- 默认值：`1.0`

## 4. 持久化

- 文件：`~/.tiangong/browser-zoom.json`
- 内容：单个 f64 数字，例如 `1.3`
- 读写时机：
  - `BrowserManager::new()` 初始化时 `load_zoom()`，文件不存在或解析失败兜底 `1.0`
  - 每次 `browser_set_zoom` 成功后 `persist_zoom()` 原子覆盖写
- 与 `browser-history.json`、`browser-data/` 相互独立，无并发冲突

## 5. 前端 UI 与快捷键

### 工具栏控件（`BrowserPanel.tsx`）

放置在批注按钮之前，与现有按钮统一尺寸风格：

```
[-]  [110%]  [+]
```

- `-` / `+`：步进调整，调用 `browserSetZoom(zoom ± 0.1)`，用返回值更新显示
- 百分比标签：双击触发重置（`browserResetZoom`）

### 快捷键（仅浏览器面板可见时生效）

| 快捷键 | 行为 |
|--------|------|
| `Cmd/Ctrl + =` 或 `+` | 放大 |
| `Cmd/Ctrl + -` | 缩小 |
| `Cmd/Ctrl + 0` | 重置 |

注册方式参考 `frontend/src/components/MessageList.tsx` 中 `Cmd+F` 的 `window.addEventListener('keydown', h, true)` capture 模式，挂载时注册、卸载时移除，并 `preventDefault()` 阻止浏览器自身缩放。

## 6. 新建/切换标签页

- `BrowserManager::create_webview_for_tab` 在 `add_child` 返回 webview 之后立即调用 `webview.set_zoom(state.zoom_factor)`
- **不能**放在 `on_page_load` 回调里，否则首屏以 100% 渲染再跳变，产生闪烁
- 切换标签页无需额外处理：每个 webview 各自维持当前缩放，全局 `zoom_factor` 保持一致

## 7. 已知平台差异

- macOS WKWebView / Windows WebView2：`set_zoom` 行为一致，渲染平滑
- Linux WebKitGTK：缩放比例 >2.0 时部分网页可能出现锯齿或字体模糊，clamp 上限 `5.0` 已是保护值；如反馈集中可后续下调上限

## 8. 与其他能力的兼容性

| 能力 | 是否受影响 | 说明 |
|------|------------|------|
| 页面内容提取（`browser_eval` / `fetch_page_content`） | 否 | zoom 只改渲染，DOM `innerText` 等逻辑值不变 |
| 元素坐标（`getBoundingClientRect`） | 否 | 返回 layout 坐标，不受视觉缩放影响 |
| 批注框选（rect / arrow） | 自动同步 | 批注 canvas 是 webview 内 DOM，随 webview 等比缩放；坐标语义不变 |
| 页面截图 | 缩放后画面 | 截取的是当前视觉状态，符合预期 |
