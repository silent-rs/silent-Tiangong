# 单 PTY 模型

> 状态：已实现
> 日期：2026-06-14

---

## 1. 设计决策

Agent 命令执行和用户面板操作共享同一个系统 PTY。所有操作通过同一 mpsc channel 串行化处理，无需路由层。

**优势**：

1. **无路由复杂度**：所有操作直达同一 channel，天然无路由不一致。
2. **Agent 命令面板可见**：用户打开终端面板即可看到 Agent 执行过的所有命令和输出。
3. **协作状态机全覆盖**：系统 PTY 的命令循环携带协作状态跟踪器，Agent 执行命令期间的用户干预可以被检测到。
4. **历史持久化统一**：一个 PTY 对应一份落盘日志，重启后回填到唯一的环形缓冲区。

**代价**：

- Agent 执行长命令时，用户无法在面板并行操作（协作状态机会阻止）
- 切换对话时终端 cwd/历史保持不变（终端跨对话共享）

---

## 2. 数据流

### 2.1 Agent 执行命令

1. Agent LLM 发出 `run_shell` tool_call
2. Core 层 `ToolOverrideHandler` 拦截，调用 `TerminalProvider::exec`
3. Provider 通过 mpsc channel 发送 `Exec` 命令到命令循环
4. `handle_exec` 设置 AgentRunning 状态，生成唯一 marker（SCRU128），发送组合命令到 PTY writer
5. 轮询输出缓冲区，检测 end marker，收集 start/end 之间的输出
6. 设置 Idle 状态，通过 oneshot channel 返回 `TerminalExecResponse`
7. Override 层封装为 `ToolResult`（stdout + summary + hints）返回给 Agent

同时，PTY 输出读取线程实时把过滤后的输出经 `RawOutputFilter` 过滤 marker 行后，一路落盘到 OutputLogger，一路通过 `terminal:output` 事件推送到前端 xterm.js。

### 2.2 用户面板操作

1. 用户在 xterm.js 中键盘输入，触发 `term.onData(data)`
2. 前端调用 `terminalSessionSendInput(sessionId, data)`
3. 后端命令循环收到 `SendInput`，标记来源为 `InputSource::User`
4. 写入 PTY writer，同时调用 `activity.record_user_input()`
5. 若 Agent 正在执行命令，标记 `user_intervened = true`
6. PTY 回显经 reader thread 推送回前端

### 2.3 run_command PTY 分流

`run_command`（非 `run_shell`）在 Core 层校验通过后也走 PTY：

1. 提取超时和交互标记
2. 执行命令白名单校验和路径安全校验
3. 校验通过后，若注入了 `terminal_provider`，进入 PTY 执行路径
4. 比较 `effective_cwd` 与 PTY 当前 cwd，不同时包装为 `cd '<cwd>' && <cmd> <args>`
5. 处理 tokio runtime 上下文（MultiThread 用 `block_in_place`，CurrentThread 新建线程 + 独立 runtime）
6. 调用 `provider.exec_command(cmd, args, timeout)`

---

## 3. 命令循环（spawn_command_loop）

命令循环是单 PTY 模型的核心枢纽。所有操作通过 mpsc channel 串行化处理：

| 命令类型 | 处理逻辑 |
|---------|---------|
| `Exec` | 调用 `handle_exec`（marker 协议、交互检测、超时处理） |
| `ExecInteractive` | 调用 `handle_exec_interactive`（直接发送、等待初始输出） |
| `RecentOutput` | 从环形缓冲区读取最近 N 行 |
| `CurrentCwd` | 返回当前工作目录 |
| `SendInput` | 写入 PTY writer；来源为 User 时记录用户活跃 |
| `SetCwd` | 更新 state.cwd + 发送 `cd '<cwd>'\r` 到 PTY |
| `Resize` | 调整 PTY 尺寸 |
| `Reset` | 清空缓冲区 + 清空日志 + 重启 PTY + 新 reader thread |

**串行化保证**：Exec 命令在等待 marker 期间会阻塞循环，后续命令排队等待。这确保了命令边界的正确检测——不会有两条命令的 marker 交叉。

---

## 4. PTY 生命周期

| 阶段 | 动作 |
|------|------|
| 应用启动 | 创建 TerminalManager → 打开 OutputLogger → 回填历史 → start_pty → spawn_output_reader → spawn_command_loop |
| 运行中 Reset | drop 旧 PTY → 清空缓冲区 → 清空日志 → start_pty → 新 reader thread |
| PTY 恢复 | `ensure_system_pty`：PTY 不可用时自动尝试重启一次 |
| 应用退出 | channel 关闭 → 命令循环退出 → drop pty_state → 子进程终止 |

---

## 5. TerminalPluginState 共享状态

| 字段 | 类型 | 说明 |
|------|------|------|
| `manager` | `Arc<TerminalManager>` | 系统 PTY 管理器（Agent + 面板共用） |
| `cmd_tx` | `mpsc::Sender<TerminalCommand>` | 命令发送端（所有 Tauri 命令通过此 channel 与命令循环通信） |
| `activity` | `Arc<TerminalActivityTracker>` | 协作状态跟踪器（用户/Agent 竞态防护） |

Tauri 命令通过 `State<'_, TerminalPluginState>` 访问共享状态。`TerminalProviderImpl` 持有 `cmd_tx` 的克隆，Core 层通过 trait 方法间接发送命令。
