# 协作状态机

> 状态：已实现
> 日期：2026-06-14

---

## 1. 问题

单 PTY 模型下，Agent 和用户共享同一个终端。如果不做协调，可能出现：

- Agent 正在执行 `npm install`，用户在面板输入 `rm -rf node_modules`，两条命令交叉
- Agent 启动了 vi，用户不知道，在 shell 提示符处输入命令
- Agent 命令超时卡住，用户无法干预

---

## 2. 状态定义

| 状态 | 说明 |
|------|------|
| `Idle` | 终端空闲，可以接受用户输入或 Agent 命令 |
| `UserActive` | 用户最近正在操作终端（2 秒内有键盘输入） |
| `AgentRunning { command_id }` | Agent 正在执行非交互命令（marker 检测中） |
| `AgentInteractive { command_id }` | 终端有前台交互进程（vi/python REPL 等） |

### 状态转移

- `Idle` → `AgentRunning`：Agent 开始执行非交互命令
- `Idle` → `UserActive`：用户键盘输入
- `AgentRunning` → `Idle`：命令执行完成
- `AgentRunning` → `AgentInteractive`：检测到交互提示，进入交互模式
- `AgentInteractive` → `Idle`：交互程序退出，Agent 命令结束
- 任何 Agent 状态下用户输入 → 标记 `user_intervened`

---

## 3. TerminalActivityTracker

| 方法 | 触发时机 | 效果 |
|------|---------|------|
| `record_user_input()` | 面板键盘输入（`InputSource::User`） | 更新 `last_user_input`；若 Agent 执行中，设置 `user_intervened = true` |
| `set_busy_state(state)` | Agent 命令开始/结束 | 更新状态；新命令开始时清除 `user_intervened` |
| `busy_state()` | 路由决策、UI 状态查询 | 返回当前状态快照 |
| `is_user_active(threshold)` | 路由决策 | 用户在 threshold 内有输入则返回 true |
| `take_user_intervened()` | Agent 命令结束 | 取出并重置干预标记，附加到命令结果 |

---

## 4. 干预检测流程

1. Agent 开始执行命令，设置 `AgentRunning` 状态，清除 `user_intervened`
2. 命令执行中，用户在面板输入 → `record_user_input()` → 检测到 Agent 执行中 → 设置 `user_intervened = true`
3. 用户输入写入 PTY（可能与 Agent 命令交叉）
4. 命令结束，取出 `user_intervened` 标记，设置 `Idle` 状态
5. `TerminalExecResponse.interrupted_by_user` 携带干预信号返回给 Agent

Agent 收到 `interrupted_by_user: true` 后，在 stderr 中看到提示：
> [提示] 命令被用户中断，建议询问用户是否需要调整执行计划

---

## 5. 前端状态查询

前端通过 `terminal_session_status` 命令获取协作状态，返回 `phase` 字段：

| phase 值 | 含义 |
|----------|------|
| `Idle` | 终端空闲 |
| `UserActive` | 用户正在操作 |
| `Running` | Agent 执行中 |
| `Interactive` | 交互程序运行中 |

用于 StatusPanel 绿点指示器。

---

## 6. Prompt 规则注入

`TerminalPromptSectionProvider` 向 system prompt 注入 12 条终端交互规则，覆盖：

| 规则类别 | 内容 |
|---------|------|
| 命令执行 | 必须用 `run_shell` 实际执行，不能仅展示 |
| 交互操作 | 先列出步骤，再用 `run_shell(interactive=true)` + `terminal_input` 分步执行 |
| 禁止事项 | 禁止 heredoc/pipe/expect 自动化交互程序 |
| 控制字符 | `\u0003`=Ctrl+C、`\u001b`=Esc、`\n`=回车 |
| 恢复策略 | 优先 `terminal_output` 查看状态 → Ctrl+C → Esc → 仅最后 `terminal_reset` |
| swap 文件 | vi 启动遇到 swap → q 退出 → rm .swp → 重新启动 |
| 记忆覆盖 | recall_memory 的"不可交互"建议已过时，当前环境支持完整交互 |
