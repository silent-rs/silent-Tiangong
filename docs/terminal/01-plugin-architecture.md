# 终端插件化架构

> 状态：已实现
> 日期：2026-06-14
> 关联：`tiangong-plugin-terminal`、`tiangong-core`、`src-tauri`、`frontend`

---

## 1. 背景

天工需要嵌入式终端能力：Agent 执行的命令在终端面板可见，用户也可以直接在面板中操作 shell。终端能力需要与 Core 解耦——CLI/Server 模式下没有终端面板，命令应回退到独立子进程。

采用与浏览器插件（`tiangong-plugin-browser`）相同的架构模式：将终端能力封装为独立的 Tauri Plugin crate，Core 层通过 trait 抽象与终端解耦。

---

## 2. 目标架构

| 层级 | 模块 | 职责 |
|------|------|------|
| **Frontend** | `TerminalPanel.tsx` | xterm.js 渲染、键盘输入、resize |
| | `MainApp.tsx` | 面板布局管理（对半分、与浏览器互斥、拖拽关闭） |
| | `api/tauri.ts` | invoke 封装 |
| **Plugin** | `lib.rs` | Plugin 注册、状态初始化、Provider 工厂 |
| | `manager.rs` | TerminalManager、PTY 生命周期、命令循环 |
| | `command_protocol.rs` | Marker 协议、exec / exec_interactive |
| | `handler.rs` | TerminalProvider impl + ToolOverride |
| | `output_processor.rs` | 输出行处理、Marker 过滤、日志器 |
| | `collaboration.rs` | 协作状态机 |
| | `commands.rs` | Tauri 命令（前端 invoke 入口） |
| | `types.rs` / `util.rs` | 共享类型 / 工具函数 |
| **Core** | `terminal_trait.rs` | TerminalProvider trait |
| | `tool_override.rs` | ToolOverrideHandler / ToolSpecProvider / PromptSectionProvider trait |
| | `tool/run_command.rs` | run_command PTY 分流（校验后路由） |
| | `runtime.rs` | tool_executor + terminal_provider 注入 |
| **src-tauri** | `main.rs` | Plugin 注册 + Provider/Override 注入到 TiangongApp |
| | `app.rs` | TiangongApp 持有 terminal_provider |

---

## 3. Core 层 Trait 抽象

### 3.1 TerminalProvider

Core 层定义 `TerminalProvider` trait，GUI 模式下由 plugin 实现，CLI/Server 模式下为 `None`（回退到独立子进程）。

| 方法 | 用途 | 调用方 |
|------|------|--------|
| `exec` | 执行 shell 脚本，使用 marker 检测命令边界 | run_shell |
| `exec_command` | 执行原始命令，自动格式化 cmd + args | run_command |
| `exec_interactive` | 交互式执行，不使用 marker，直接发送并等待初始输出 | run_shell (interactive=true) |
| `exec_command_interactive` | 交互式执行原始命令 | run_command (interactive) |
| `recent_output` | 获取终端最近 N 行输出 | terminal_output |
| `current_cwd` | 获取当前工作目录 | run_command cwd 包装 |
| `send_input` | 发送输入到终端 stdin | terminal_input |
| `reset` | 重置终端会话（重启 shell） | terminal_reset |

`TerminalExecResult` 携带退出码、stdout/stderr、cwd、超时/中断/交互模式标记。

### 3.2 工具覆盖 Trait

Plugin 通过三个额外 trait 向 Core 注入能力：

| Trait | 作用 | 实现类 |
|-------|------|--------|
| `ToolOverrideHandler` | 拦截 `run_shell` / `terminal_input` / `terminal_output` / `terminal_reset`，路由到 PTY | `TerminalToolOverride` |
| `ToolSpecProvider` | 向 LLM 注册 `terminal_input` / `terminal_output` / `terminal_reset` 工具定义 | `TerminalToolSpecProvider` |
| `PromptSectionProvider` | 向 system prompt 注入终端交互规则（分步操作、恢复策略、swap 文件处理等） | `TerminalPromptSectionProvider` |

---

## 4. Plugin 初始化与注入流程

### 应用入口（src-tauri/main.rs）

1. Tauri Builder 注册 `tiangong_plugin_terminal::init(session_id, cwd)`
2. setup 阶段获取 Plugin 暴露的 Provider 和 Override
3. 将 Provider 注入 `TiangongApp`（Core 层可访问）
4. 注册工具覆盖：`run_shell` / `terminal_input` / `terminal_output` / `terminal_reset` 走 PTY
5. 注册工具定义：LLM 可见 `terminal_input` / `terminal_output` / `terminal_reset`
6. 注册 Prompt 规则：终端交互引导
7. 异步同步 workspace cwd 到系统 PTY

### Plugin init 内部流程

1. 创建 `TerminalManager`，打开持久化日志，回填历史到环形缓冲区
2. 启动系统 PTY + 输出读取线程
3. 创建协作状态跟踪器 `TerminalActivityTracker`
4. 注册 `TerminalPluginState` 共享状态（manager + cmd_tx + activity）
5. 启动命令处理循环（携带 activity）

---

## 5. Crate 依赖关系

| 依赖 | 用途 |
|------|------|
| `tiangong-core` | TerminalProvider trait、ToolResult、ToolCall |
| `tiangong-types` | 共享类型 |
| `portable-pty` | PTY 子进程管理 |
| `tauri` | Plugin 框架 |
| `tokio` | async runtime、channel |
| `scru128` | Marker ID 生成 |
| `tracing` | 日志 |

Core 不依赖 Plugin——Core 只定义 trait，Plugin 实现 trait 并在运行时注入。

---

## 6. 模块职责

| 模块 | 职责 |
|------|------|
| `lib.rs` | Plugin 注册、状态初始化、Provider/Override 工厂函数 |
| `manager.rs` | TerminalManager（PTY 生命周期）、命令循环、start_pty |
| `command_protocol.rs` | handle_exec（marker 协议）、handle_exec_interactive、collect_command_output |
| `handler.rs` | TerminalProviderImpl、TerminalToolOverride、ToolSpecProvider、PromptSectionProvider |
| `output_processor.rs` | TerminalLineProcessor、RawOutputFilter、OutputLogger、输出读取线程 |
| `collaboration.rs` | TerminalBusyState、InputSource、TerminalActivityTracker |
| `commands.rs` | Tauri 命令（前端 invoke 入口） |
| `types.rs` | TerminalCommand enum、TerminalExecResponse、PtyState 等 |
| `util.rs` | shell_quote |

---

## 7. 与浏览器插件的架构对比

| 维度 | Browser Plugin | Terminal Plugin |
|------|---------------|-----------------|
| Tauri Plugin | ✅ | ✅ |
| Core trait | `PageFetcher` | `TerminalProvider` |
| 工具覆盖 | `web_fetch` / `web_click` / ... | `run_shell` / `terminal_input` / ... |
| 工具定义 | 内置 | `ToolSpecProvider` 动态注册 |
| Prompt 注入 | ❌ | ✅ `PromptSectionProvider` |
| 子进程管理 | WebView | PTY (portable-pty) |
| 输出处理 | DOM 提取 | ANSI 解析 + 行处理 + Marker 过滤 |
| 历史持久化 | ❌ | ✅ OutputLogger |
