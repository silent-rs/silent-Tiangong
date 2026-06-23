# 11 - 浏览器 Tab 内容组件

## 目标

把浏览器面板拆成单个浏览器 Tab 内容组件，服务统一 Tabs 容器。

## 范围

- `frontend/src/components/BrowserTabContent.tsx`
- `frontend/src/api/tauri.ts`

## 任务

- 组件 props 包含 `tabId`、`initialUrl`、`isActive`。
- 活跃时调用 `browser_tab_switch(tabId)`。
- 活跃时同步 WebView 位置。
- 地址栏导航到真实 URL 时调用 `browser_open` 或 `browser_navigate`。
- `about:blank` 不创建 WebView。
- 保留已有前进、后退、刷新、缩放、批注、历史入口。

## 不做

- 不改浏览器后端会话切换。
- 不重写历史功能。

## 验收

- 切换浏览器 Tab 后显示对应 WebView。
- 空白 Tab 输入 URL 后正常打开。
- 缩放和批注能力保留。

## 验证

- `yarn build`
