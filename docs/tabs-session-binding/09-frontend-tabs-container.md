# 09 - 统一 Tabs 容器

## 目标

新增统一 Tabs 容器，承载浏览器和终端 Tab 的混排、切换、新建、关闭。

## 范围

- `frontend/src/components/TabsContainer.tsx`
- `frontend/src/api/tauri.ts`

## 任务

- 定义前端 `TabState` 类型。
- 渲染统一 Tab 栏。
- Tab 标题显示类型图标和标题。
- 支持新建浏览器 Tab。
- 支持新建终端 Tab。
- 支持切换 Tab。
- 支持关闭 Tab。
- 关闭最后一个 Tab 时关闭工作区面板。

## 不做

- 不做会话恢复持久化。
- 不实现具体浏览器/终端内容组件细节。

## 验收

- 浏览器和终端可在同一个 Tab 栏混排。
- 新建 Tab 后立即激活。
- 关闭活跃 Tab 后切换到相邻 Tab。

## 验证

- `yarn build`
