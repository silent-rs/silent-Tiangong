# 05 - 命令结果反馈终端选择信息

## 目标

让 Agent 从工具结果中知道本次命令使用了哪个终端，以及是否因为繁忙而新建终端执行。

## 范围

- `crates/tiangong-core/src/tool/run_command.rs`
- `crates/tiangong-plugin-terminal/src/handler.rs`
- 必要时扩展 `TerminalExecResult`

## 依赖

- 前置任务：04。
- 后续任务：14。
- 可并行任务：07、08、09、11。
- 阻塞说明：必须先有终端选择结果，才能把复用或新建终端的信息反馈给 Agent。

## 任务

- 将终端选择信息传入命令执行结果整理阶段。
- 当复用旧终端时，结果中说明使用的终端 id。
- 当新建终端时，结果中说明：
  - 本次创建了新终端。
  - 新建原因：无可用终端 / 所有终端繁忙。
  - 没有写入旧终端。
- 命令成功、失败、超时都要保留终端选择信息。
- 不污染原始 stdout。
- 优先放在 `summary`；必要时补充到 `stderr` 的提示段。

## 不做

- 不新增 UI。
- 不改变命令 stdout/stderr 截断策略。

## 验收

- 复用终端时 Agent 能看到终端 id。
- 新建终端时 Agent 能看到新建原因。
- 命令失败时仍能看到终端选择信息。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
