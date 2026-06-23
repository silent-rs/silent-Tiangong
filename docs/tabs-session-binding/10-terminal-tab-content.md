# 10 - 终端 Tab 内容组件

## 目标

把终端渲染拆成单个 Tab 内容组件，并通过复合 id 路由到对应终端 PTY。

## 范围

- `frontend/src/components/TerminalTabContent.tsx`
- `frontend/src/api/tauri.ts`

## 任务

- 组件 props 包含 `sessionId`、`tabId`、`isActive`。
- 使用复合 id：`sessionId:tabId`。
- xterm 输出只消费匹配复合 id 的 `terminal:output`。
- 用户输入发送到对应复合 id。
- resize、cwd、screen snapshot 都按复合 id 调用。
- 用户命令上报按复合 id 注入。

## 不做

- 不实现统一 Tab 栏。
- 不改后端 PTY 协议。

## 验收

- 两个终端 Tab 的输出不会串写。
- 切换 Tab 后 xterm 尺寸能重新适配。
- 用户输入只进入当前终端 Tab。

## 验证

- `yarn build`
