# 09 - 统一 Tabs 容器

## 目标

新增统一 Tabs 容器，承载浏览器和终端 Tab 的混排、切换、新建、关闭。

## 范围

- `frontend/src/components/TabsContainer.tsx`
- `frontend/src/api/tauri.ts`

## 依赖

- 前置任务：01、02、08。
- 后续任务：10、11、12、14。
- 可并行任务：05、07。
- 阻塞说明：统一容器需要会话 Tab 类型、读写 API 和单一工作区面板入口。

## 任务

- 定义前端 `TabState` 类型。
- 渲染统一 Tab 栏。
- Tab 标题显示类型图标和标题。
- 支持新建浏览器 Tab。
- 支持新建终端 Tab。
- 支持切换 Tab。
- 支持关闭 Tab。
- 关闭最后一个 Tab 时关闭工作区面板。

## Tab 栏 UX 改进（2026-09 追加）

- Tab 栏采用 flex 布局：滚动区 `flex-1 min-w-0` 承载溢出（横向滚动），
  右侧「关闭工作区」按钮独立 `shrink-0` 容器，Tab 再多也始终可见；
  滚动区 `overflow-y-hidden` + `custom-scrollbar`（悬浮显示滚动条，不常驻占位）。
- 鼠标滚轮（垂直）在 Tab 栏上转为横向滚动；触控板横向滚动保持原生行为。
- 活跃 Tab 变化时自动滚入可视区（`data-tab-id` + `scrollIntoView`）。
- Tab 右键菜单新增「关闭所有同 App 标签页」：同 App = 同插件同贡献点
  （`plugin_id` + `contribution_id`，如浏览器的多个标签页、多例三方 App），
  仅当同 App 实例数 > 1 时显示；逐个走 `handleCloseTab`（含 webview 运行时关闭）。

## 不做

- 不做会话恢复持久化。
- 不实现具体浏览器/终端内容组件细节。

## 验收

- 浏览器和终端可在同一个 Tab 栏混排。
- 新建 Tab 后立即激活。
- 关闭活跃 Tab 后切换到相邻 Tab。
- Tab 数量超出宽度时右侧「关闭工作区」按钮始终可见。
- 鼠标滚轮可横向滚动 Tab 栏。
- 右键可一次关闭同 App 的全部标签页。

## 验证

- `yarn build`
