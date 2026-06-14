# 命令协议

> 状态：已实现
> 日期：2026-06-14

---

## 1. Marker 边界检测

### 1.1 问题

PTY 是一个双向字节流，没有原生的"命令结束"信号。Agent 发送一条命令后，需要知道何时输出结束、退出码是什么、当前 cwd 在哪里。

### 1.2 方案：四 Marker 协议

每次 `exec` 发送一组组合命令，用唯一 ID 关联四个 marker：

| Marker | 格式 | 作用 |
|--------|------|------|
| START | `__TIANGONG_START_<scru128>__` | 命令输出起始边界 |
| END | `__TIANGONG_END_<scru128>__` | 命令输出结束边界 |
| CWD | `__TIANGONG_CWD_<scru128>__` | 命令执行后的工作目录 |
| RC | `__TIANGONG_RC_<scru128>__` | 命令退出码 |

使用 SCRU128 生成唯一 ID，避免短时间内的 marker 碰撞。

组合命令的发送格式为：先 echo START marker，然后执行用户命令（禁用 pager），接着捕获退出码、pwd、echo CWD/RC/END marker。`PAGER=cat` 和 `GIT_PAGER=cat` 确保所有命令输出不分页。

### 1.3 检测流程

1. 发送组合命令前，记录 `total_lines_pushed`（命令起点）
2. 轮询循环（每 100ms）读取新增行，检测 START / END / CWD / RC
3. START + END 都出现 → 收集区间输出，返回结果
4. START 出现但 END 未出现，且有可见输出：
   - 检测到交互提示（Password:、>>>、[y/n] 等）→ 进入交互模式
   - 8 秒兜底超时 → 返回当前终端显示内容
5. 总超时（默认 120s）→ 发送 Ctrl+C + CR → 返回已捕获输出

### 1.4 输出收集

`collect_command_output` 在环形缓冲区中扫描 START/END 区间。

**兜底机制**：如果 START marker 被环形缓冲区淘汰（缓冲区上限 5000 行），使用 `fallback_start_idx` 限制收集范围，避免上次命令的残留输出混入。索引通过 `buf_idx_from_pushed` 从 `total_lines_pushed` 反推。

---

## 2. 交互式命令检测

### 2.1 启发式提示符识别

`looks_like_interactive_prompt` 检测终端最后一行是否为交互提示：

- 精确匹配 shell 提示符：`>>>` / `...` / `>` / `$` / `%` / `#`
- 以 `$` `%` `#` 结尾
- 以 `?` 结尾且为短行（≤80 字符）或含 yes/no 关键词
- 包含 password / passphrase / username / login / OTP 等关键词
- 以 `:` 结尾且含 password / code / input / select 等关键词

### 2.2 交互模式触发条件

当 START marker 已出现但 END marker 未出现时：

| 条件 | 触发动作 |
|------|---------|
| 输出稳定 700ms + 匹配交互提示 | 进入交互模式，返回当前屏幕内容 |
| START 后 8 秒兜底 | 进入交互模式，返回当前屏幕内容 |
| 总超时（默认 120s） | 发送 Ctrl+C，返回已捕获输出 |

进入交互模式后，协作状态转为 `AgentInteractive`，Agent 通过 `terminal_input` 继续操作。

---

## 3. 交互式命令执行（handle_exec_interactive）

交互式命令不使用 marker，直接发送命令并等待初始输出：

1. 清理残留进程：发送 Ctrl+C + Ctrl+U（清行）
2. 发送命令，用 `\r`（CR）而非 `\n`（LF）作为回车
3. 等待指定秒数的初始输出
4. 收集输出时过滤掉内部 marker 行，避免上次 exec 残留泄漏

---

## 4. 输入转义解码

### 4.1 问题

LLM 发送的输入字符串可能包含字面转义序列（如 `\u0003` 这 6 个字符），需要解码为真实的控制字节（0x03）。否则在 vi 等交互程序的插入模式下会变成可见文本。

### 4.2 解码规则

`decode_terminal_input` 分两阶段处理：

**第一阶段：字面转义解码**

| 输入字面 | 解码为 | 说明 |
|---------|--------|------|
| `\uHHHH` | Unicode 标量 | 4 位十六进制 |
| `\xHH` | 字节值 | 2 位十六进制 |
| `\e` / `\E` | 0x1b (Esc) | 短格式 |
| `\n` / `\r` / `\t` / `\0` | 对应控制字符 | 容错 |
| `\\` | 0x5c (\) | |
| `^C` | 0x03 (Ctrl+C) | Caret 记法 |
| `^[` | 0x1b (Esc) | |
| `^M` | 0x0d (CR) | |

**第二阶段：LF → CR 映射**

所有 `\n`（0x0a）替换为 `\r`（0x0d）。PTY 线路规程用 ICRNL 把 CR 转为 LF 喂给应用，但 zsh ZLE / vim / less 等 raw 模式程序只识别 CR 作为"提交本行"。

---

## 5. 交互脚本拦截

### 5.1 问题

LLM 可能尝试用 heredoc、管道、`vi -es` 等批处理模式自动化交互程序，绕过 `terminal_input` 分步操作。这通常会导致乱码或数据丢失。

### 5.2 检测策略

同时满足以下两个条件时拒绝执行：

1. 脚本中调用了交互式程序（vi/vim/nvim/nano/ssh/sftp/ftp/python/python3/node/irb/psql/mysql/sqlite3/expect）
2. 脚本使用了 stdin 自动化模式（heredoc `<<`、管道 `|`、`printf`/`echo` 喂输入、`-es` ex 模式、`--cmd`）

拒绝时返回错误信息，引导 Agent 改用 `run_shell(interactive=true)` + `terminal_input` 分步操作。

---

## 6. Marker 过滤（RawOutputFilter）

### 6.1 双输出路径

PTY 输出同时走向两个消费者，需要不同的过滤策略：

| 消费者 | 过滤策略 | 实现 |
|--------|---------|------|
| xterm.js（前端面板） | **过滤 marker 行**，正常输出实时透传 | `RawOutputFilter` |
| 环形缓冲区（Agent 读取） | **保留 marker 行**（exec 需要检测 marker） | `TerminalLineProcessor` |

### 6.2 RawOutputFilter 算法

1. 输入 chunk 追加到 pending 缓冲区
2. 完整行（含 `\n`）：检查是否包含 marker，包含则丢弃，否则输出
3. 不完整行（无 `\n`）：
   - 含 marker 或 marker 前缀 → 暂存等待换行确认（超过 8KB 强制输出）
   - 不含 marker → 检查尾部是否为 marker 前缀（`__TIANGONG_`），是则在前缀前切分输出，否则全部输出

`safe_split_point` 确保切分点在 UTF-8 字符边界上，避免多字节字符被拆开。

---

## 7. 终端行处理器（TerminalLineProcessor）

模拟光标行为处理 zsh 行编辑器的重绘，为环形缓冲区提供"逻辑行"：

| 处理的 CSI 序列 | 效果 |
|----------------|------|
| `\x1b[K` | Erase in Line：从光标处清除到行尾 |
| `\x1b[G` | Cursor Horizontal Absolute：设置光标列 |
| `\x1b[J` | Erase in Display：n≥2 时清屏 |
| `\x1b[C` | Cursor Forward：光标右移 |
| `\x1b[D` | Cursor Back：光标左移 |
| `\x1b[P` | Delete Character：删除光标处字符 |
| `\x1b[@` | Insert Character：在光标处插入空格 |

还处理 OSC 序列（`\x1b]...BEL`）和字符集切换（`\x1b(` / `\x1b)`）。跨 chunk 的不完整 ESC 序列暂存在 `pending` 缓冲区，下次输入时拼接。
