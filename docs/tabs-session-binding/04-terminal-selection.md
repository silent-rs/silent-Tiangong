# 04 - 终端空闲选择与繁忙新建

## 目标

`run_command` / `run_shell` 执行前自动选择可用终端：优先复用空闲终端，所有终端繁忙时新建终端。

## 范围

- `crates/tiangong-core/src/terminal_trait.rs`
- `crates/tiangong-core/src/tool/run_command.rs`
- `crates/plugins/tiangong-plugin-terminal/src/session_pty.rs`
- `crates/plugins/tiangong-plugin-terminal/src/handler.rs`

## 依赖

- 前置任务：03。
- 后续任务：05、10、14。
- 可并行任务：06、08。
- 阻塞说明：必须先支持同一会话多终端运行时，才能实现空闲选择和繁忙新建。

## 任务

- 定义终端选择结果结构：
  - `session_id`
  - `tab_id`
  - `created_new`
  - `reason`
- 在终端 provider 中增加“为命令选择终端”的能力。
- 选择策略：
  - 过滤死亡 PTY。
  - 优先选择空闲 PTY。
  - 没有 PTY 时创建新 PTY。
  - 全部繁忙时创建新 PTY。
- `run_command` 通过选择结果中的复合 id 执行。
- `run_shell` 也使用相同选择策略。
- `terminal_send` 默认写入当前活跃/选中的可用终端。

## 忙闲判定

- 复用现有 `TerminalActivityTracker`。
- 执行命令、交互命令、等待用户输入、前台进程未退出均视为繁忙。
- shell idle 且 PTY 存活视为空闲。

## 不做

- 不改前端 Tab 容器。
- 不改浏览器。
- 不改变命令安全校验。

## 验收

- 有一个空闲终端时，命令复用该终端。
- 无终端时，命令创建新终端。
- 终端繁忙时，命令创建新终端，不写入繁忙终端。
- `run_command` 和 `run_shell` 行为一致。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
