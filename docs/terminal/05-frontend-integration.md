# 前端集成

> 状态：已实现
> 日期：2026-06-14

---

## 1. 组件结构

| 组件 | 职责 |
|------|------|
| `MainApp.tsx` | 面板布局管理：对半分、与浏览器互斥、拖拽关闭 |
| `AppSidebar.tsx` | 终端切换按钮 |
| `TerminalPanel.tsx` | xterm.js 渲染、键盘输入、resize、工具栏 |

TerminalPanel 内部包含：
- **工具栏**：cwd 显示 + 重置按钮 + 关闭按钮
- **xterm.js 容器**：终端渲染区域

---

## 2. 单 PTY 会话获取

面板挂载时通过 `terminalSystemSessionInfo` 获取系统 PTY 的 session_id。后续所有操作（输入、输出、resize、reset）都使用这个固定 ID。获取失败时回退到硬编码 fallback ID。

---

## 3. xterm.js 配置

| 配置项 | 值 | 说明 |
|--------|-----|------|
| `cursorBlink` | true | 光标闪烁 |
| `fontSize` | 13 | 字号 |
| `fontFamily` | Menlo, Monaco, Courier New | 等宽字体 |
| `convertEol` | false | PTY 已处理换行，不需要 xterm 转换 |
| `scrollback` | 10000 | 滚动缓冲行数 |
| `theme.background` | `#1e1e2e` | Catppuccin Mocha 暗色主题 |

---

## 4. 事件流

### 4.1 实时输出

1. 后端 PTY reader thread 收到输出 chunk
2. `RawOutputFilter` 过滤 marker 行
3. 通过 `app.emit("terminal:output")` 发送 Tauri 事件
4. 前端 `listen("terminal:output")` 监听，按 `session_id` 匹配 xterm 实例
5. 调用 `term.write(text)` 渲染到 DOM

### 4.2 用户键盘输入

1. 用户在 xterm.js 中按键，触发 `term.onData(data)`
2. 前端调用 `api.terminalSessionSendInput(sessionId, data)`
3. 后端写入 PTY writer，标记来源为 `InputSource::User`
4. PTY 回显经 reader thread 推送回前端

### 4.3 尺寸自适应

通过 `ResizeObserver` 监听容器尺寸变化，自动调用 `fitAddon.fit()` 计算 cols/rows，触发 `term.onResize` 通知后端调整 PTY 尺寸。

### 4.4 历史输出加载

首次创建 xterm 实例时，从后端环形缓冲区加载最近 5000 行历史，将 `\n` 转为 `\r\n` 让 xterm 正确换行。

---

## 5. 布局管理（MainApp.tsx）

### 5.1 面板开关

- **打开**：若浏览器面板已打开则先关闭（互斥）→ 收起侧边栏 → 对半分宽度 → 显示终端
- **关闭**：隐藏终端 → 若用户偏好侧边栏打开则恢复

终端面板使用对半分布局（区别于浏览器面板的固定 400px 宽度）。

### 5.2 拖拽关闭

拖拽分隔条到右侧剩余宽度小于面板最小宽度时，自动关闭当前打开的面板（浏览器优先，否则终端）。

---

## 6. 前端 API

| API 方法 | 后端命令 | 说明 |
|---------|---------|------|
| `terminalSystemSessionInfo()` | `terminal_system_session_info` | 获取系统 PTY 的 session_id/cwd/shell/alive |
| `terminalEnsureSession(id, cwd)` | `terminal_ensure_session` | 确认 PTY 存活（单 PTY 下恒返回 alive） |
| `terminalSessionSendInput(id, data)` | `terminal_session_send_input` | 发送用户键盘输入 |
| `terminalSessionRecentOutput(id, lines)` | `terminal_session_recent_output` | 获取历史输出 |
| `terminalSessionResize(id, cols, rows)` | `terminal_session_resize` | 调整 PTY 尺寸 |
| `terminalSessionReset(id)` | `terminal_session_reset` | 重置终端（重启 shell） |
| `terminalSessionStatus(id)` | `terminal_session_status` | 获取状态（含协作 phase） |
| `terminalSessionSetCwd(id, cwd)` | `terminal_session_set_cwd` | 设置工作目录 |
| `terminalDestroySession(id)` | `terminal_destroy_session` | 销毁会话（单 PTY 下 no-op） |
| `terminalPanelSetSession(id)` | `terminal_panel_set_session` | 设置面板会话（单 PTY 下 no-op） |
| `terminalListStatuses()` | `terminal_list_statuses` | 列出所有会话状态 |
