# 07 - 浏览器空白页懒创建

## 目标

避免为 `about:blank` 提前创建 WebView，空白 Tab 只保存元数据，首次真实导航时再创建 WebView。

## 范围

- `crates/tiangong-plugin-browser/src/manager.rs`
- `frontend/src/components/BrowserTabContent.tsx`（拆分后）

## 任务

- `browser_tab_new("about:blank")` 只创建 Tab 元数据。
- 会话恢复遇到 `about:blank` 时不创建 WebView。
- 前端渲染空白浏览器 Tab 时不调用 `browser_open`。
- 用户输入真实 URL 导航时再调用 `browser_open` 创建 WebView。

## 不做

- 不重写历史面板。
- 不改变非空 URL 的创建流程。

## 验收

- 新建空白浏览器 Tab 不触发 WebView 创建异常。
- 空白 Tab 输入 URL 后页面正常打开。
- 会话恢复空白 Tab 后仍可首次导航。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
- `yarn build`
