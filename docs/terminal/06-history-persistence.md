# 历史持久化

> 状态：已实现
> 日期：2026-06-14

---

## 1. 目标

应用重启后，终端面板应显示上次的命令历史，让用户能回顾之前的操作。系统 PTY 是跨会话共享的常驻终端，其输出应持久化到磁盘。

---

## 2. 日志文件

| 属性 | 值 |
|------|-----|
| 路径 | `~/.tiangong/terminal.log` |
| 格式 | 纯文本（marker 过滤后的输出，与 xterm.js 看到的一致） |
| 上限 | 1 MiB（超过后保留尾部一半，防无限增长） |

---

## 3. OutputLogger

| 方法 | 说明 |
|------|------|
| `open(path)` | 创建/打开日志文件（append + read 模式），失败返回 None（优雅降级） |
| `append(text)` | 追加写一段文本，超过 1 MiB 时触发滚动 |
| `clear()` | 清空文件（用户主动 reset 时调用） |
| `path()` | 返回文件路径（回填/调试用） |

---

## 4. 日志滚动（rotate_tail）

当日志文件超过 1 MiB 时，保留尾部一半重写。

**UTF-8 安全处理**：`len / 2` 是任意字节偏移，可能落在多字节字符中间。逐字节跳到下一个 `\n` 后再读取，确保从完整的 UTF-8 行首开始，避免 `read_to_string` 因无效 UTF-8 返回 `InvalidData`。

---

## 5. 启动回填

应用启动时，从日志文件读取末尾最多 5000 行，回填到环形缓冲区。回填调用 `push_output`，保证缓冲区计数（`total_lines_pushed`）与正常写入一致。回填的历史行不包含 marker（日志只记录过滤后的输出），不会干扰后续 `handle_exec` 的 marker 扫描。

---

## 6. 写入时机

PTY 输出读取线程在每次收到 chunk 时，经过 `RawOutputFilter` 过滤后同时做两件事：

1. **落盘**：`OutputLogger.append(filtered)` — 内容与 xterm.js 看到的完全一致
2. **推送前端**：`app.emit("terminal:output", filtered)`

---

## 7. 重置时清空

用户主动 reset 终端时，日志文件也一并清空（`manager.clear_log()`），确保下次启动不会回填已废弃的历史。

---

## 8. 数据流全景

PTY 输出读取线程收到原始 chunk 后：

1. `RawOutputFilter.filter(chunk)` → 过滤 marker 行，产出纯文本
2. 纯文本同时走向三个消费者：
   - **OutputLogger.append()** → 落盘到 `~/.tiangong/terminal.log`
   - **app.emit("terminal:output")** → 推送到前端 xterm.js 渲染
   - **TerminalLineProcessor.process()** → 写入环形缓冲区供 Agent 读取
3. 应用重启时：`read_log_tail(5000)` 从日志读取末尾 → `backfill_line` 回填到环形缓冲区 → 前端加载历史写入 xterm.js
