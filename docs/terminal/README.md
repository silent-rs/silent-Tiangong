# 终端集成

天工嵌入式终端的插件化设计、PTY 协议、协作状态机与前端集成方案。

## 文档索引

| 文档 | 说明 |
|------|------|
| [01-plugin-architecture.md](./01-plugin-architecture.md) | 插件化架构：Tauri Plugin crate 结构、Core trait 解耦、注入机制 |
| [02-single-pty-model.md](./02-single-pty-model.md) | 单 PTY 模型：Agent 与面板共享终端会话的设计决策与数据流 |
| [03-command-protocol.md](./03-command-protocol.md) | 命令协议：Marker 边界检测、退出码捕获、交互式检测、输入转义解码 |
| [04-collaboration.md](./04-collaboration.md) | 协作状态机：用户/Agent 竞态防护、干预检测、忙碌状态路由 |
| [05-frontend-integration.md](./05-frontend-integration.md) | 前端集成：xterm.js 面板、事件监听、布局管理、会话切换 |
| [06-history-persistence.md](./06-history-persistence.md) | 历史持久化：OutputLogger 落盘、启动回填、日志滚动 |

## 已实现的能力

- **嵌入式终端面板**：xterm.js 渲染，用户可直接在面板中操作 shell
- **Agent 命令走 PTY**：`run_shell` / `run_command` 校验通过后在嵌入式终端执行，用户可见
- **交互式程序支持**：vi/vim/nano/ssh/python REPL 等通过 `terminal_input` 分步驱动
- **协作状态机**：Agent 执行命令期间检测用户干预，防止竞态冲突
- **终端历史持久化**：系统 PTY 输出落盘（`~/.tiangong/terminal.log`），重启后自动回填
- **输出处理**：ANSI/OSC 序列解析、zsh 行编辑器重绘模拟、Marker 行级过滤
- **交互脚本拦截**：阻止 LLM 用 heredoc/pipe 自动化 vi 等交互程序，强制分步操作
