# 持久化 Subagent 插件需求整理

需求来源：[issue #480](https://github.com/silent-rs/tiangong/issues/480)《设计持久化 Subagent 插件：跨会话激活、统一 Adapter、Hook 回调与生命周期管理》。本文档整理阶段一（最小闭环）的实施范围与关键决策，后续阶段推进时同步更新。

## 阶段一范围

- 创建 Subagent 插件（`plugins/tiangong-plugin-subagent`，插件 id `subagent`）与 `extension.tab` 管理页；
- 官方原生 Rust Sidecar 作为统一交互总线；
- 支持 `~/.tiangong/agents/<agent-id>/{agent.toml, instructions.md, memory/, artifacts/}` 身份目录；
- Agent 列表、会话激活与 Workspace 绑定（含写互斥）；
- 发送消息（`send_agent_message`）与提交任务（`submit_agent_task`）；
- 完成、阻塞、失败 Hook 回调投回激活会话；
- 关键回调先落盘再投递，Sidecar 重启后补投；
- 退出天工时清理 managed 运行实例（attached 只断开）。

不在阶段一：天工原生 Subagent/已有会话 Adapter（阶段二）、Claude Code/Codex 接入（阶段三）、OctoLoop（阶段四）。

## 关键决策

| 决策点 | 结论 | 依据 |
| --- | --- | --- |
| 插件形态 | schema v2：manifest 声明工具 + extension.tab + resident Rust sidecar，工具直连 sidecar | terminal 同款成熟形态 |
| 首版运行后端 | CLI Adapter（JSONL 非交互协议子进程，managed 进程组） | issue 接入优先级第 2 层；同时是阶段三 Claude Code/Codex 的地基 |
| Hook 投递通道 | HTTP `POST /api/v1/messages`（connector=server-api，channel_id=激活会话），`Prefer: respond-async` | scheduler/bot 已验证的正式通道，无需修改会话数据库 |
| 运行态持久化 | `~/.tiangong/agents-runtime/`（activations、tasks、runs、hooks 队列、worktrees） | 高频运行状态不写入 agent.toml |
| 存储根定位 | `TIANGONG_STORAGE_ROOT` > `$HOME/.tiangong` | 与 scheduler 一致 |
| Workspace 策略 | read-only / read-write-exclusive（写锁表强制互斥）/ isolated-worktree（git worktree） | 读可并行、写有明确所有权 |
| 会话归属 | AI 工具调用以宿主注入的 invocation context 为准，UI 操作显式传 session_id | 会话真相源纪律 |
| 退出清理 | 宿主退出流程先请求 sidecar 优雅关闭（中断 managed run → 落盘 → 标 interrupted），再走 sidecar 终止；异常退出靠进程组级联 + 子进程 stdin EOF 自退出 + Run 存活扫描恢复 | 宿主 stdio stop 为 SIGKILL 语义，无优雅窗口 |
| 子进程信号 | CLI 子进程不 setsid（留在 sidecar 进程组，宿主杀组级联清理）；运行期控制走 JSONL 协议帧 + stdin EOF；sidecar 侧信号尽力而为，失败降级 | Seatbelt 默认拒绝 process-signal（macOS 26 实测 EPERM），沙箱内组信号不可用；已在真实 Launcher 沙箱实测级联清理有效 |

## CLI Adapter JSONL 协议（首版）

- sidecar 以 `sh -c <command>`（Windows `cmd /C`）启动子进程，cwd 为绑定的 workspace（或独立 worktree）；子进程留在 sidecar 进程组内（宿主退出时级联清理）；
- stdin 每行一个 JSON：`begin`（含 agent/activation/task 信息）→ 后续 `user_message` / `interrupt` / `cancel`；
- stdout 每行一个 JSON 事件：`message` / `status` / `blocked` / `approval_required` / `completed` / `failed`；非 JSON 行按 `message` 处理；
- stderr 收进 run 日志；协议要求子进程在 stdin EOF 时自行退出（宿主异常退出的级联兜底）。

## 数据目录

```text
~/.tiangong/agents/<agent-id>/
├── agent.toml          # 身份、后端、Workspace 策略、启用状态
├── instructions.md     # 长期职责与工作要求
├── memory/             # 长期记忆（首版由 Agent 后端自行维护）
└── artifacts/          # 历史产物

~/.tiangong/agents-runtime/
├── activations.json    # 会话激活关系
├── tasks/<task_id>.json
├── runs/<run_id>.json
├── hooks/queue/<event_id>.json   # 待投递 Hook（投递成功即移除）
└── worktrees/<activation_id>/    # isolated-worktree 策略产物
```
