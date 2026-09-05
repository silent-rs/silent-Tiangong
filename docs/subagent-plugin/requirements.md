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

### 0.2.0 追加（用户裁定优先级：关联会话后端提前实现）

- **天工原生 Subagent 后端（agent_team，推荐默认）**：系统为 Agent 新建并独占一个专属天工运行时会话——
  - 招募（create_agent）无需任何会话或命令线索，backend 缺省即原生；
  - 首次运行自动新建专属会话并在投递成功后回写绑定，后续任务延续同一会话的上下文（原生 Subagent 的会话记忆）；
  - 执行与回报复用会话后端管线（消息投递 + WASM turn 钩子归因）；专属会话被删则下次运行自动重建；
  - 支持从专属会话整理长期记忆。
- **天工会话后端（tiangong_session，按需使用）**：关联用户已有会话作为 Agent 运行后端——
  - agent.toml 关联 `session_id`；管理页创建时从会话列表选择；
  - 发送消息/提交任务 = 经本机 server 向关联会话投递（消息携带任务目标、Workspace 与 Agent 长期指令/记忆摘要）；
  - **完成回报**：插件 WASM 逻辑层在 `on_turn_finished` 钩子中把「关联会话本轮最终回复」转发给 sidecar，sidecar 将对应 Run 置完成并经 Hook 投回激活会话；
  - 中断/取消 = 向关联会话投递停止通知（尽力语义，无法硬取消宿主内 turn）；
  - 记忆整理来源即关联会话历史。
- **AI 招募（create_agent 工具）**：主 Agent 对话中动态创建持久 Subagent（或复用同名成员），支持 CLI 后端（提供启动命令）与天工会话后端（session_id 或按标题搜索 session_query），默认创建后立即在当前会话激活。
- **memory 长期记忆落成实际功能**：
  - `memory/` 下 markdown 文件可管理（列表/查看/编辑/删除）；
  - 运行注入：CLI 后端 begin 帧、会话后端投递消息均携带记忆摘要（不复制全部会话历史）；
  - 自动归档：Run 完成后把「任务目标 + 结果」追加到 `memory/tasks.md`（结论式记忆）；
  - 从关联会话整理：提取每轮「用户消息 + 最终回复（截断）」生成 `memory/session-notes.md`；
  - AI 工具 `get_agent_memory` / `append_agent_memory` 读写记忆。

不在本版：天工原生 agent-team Subagent 适配（原阶段二其余部分）、Claude Code/Codex 接入（阶段三）、OctoLoop（阶段四）。

### 0.2.6 追加（用户裁定：Sub Agent 互相通信，组成集群协同作业）

- **成员间定向协作消息**：`send_agent_message` 在「发起会话恰好是某个成员的后端会话（专属会话/关联会话）」时识别为该成员发起的集群协作——
  - 目标成员无需在发起会话激活（集群协作不经过主会话激活语义），运行以合成协作激活执行；
  - 运行记录新增 `origin_session`（发起方会话）：完成/失败回报投回发起方会话，主会话发起时保持原行为（投回激活会话）；
  - 运行 Workspace 解析顺序：目标成员最近激活 → 发起成员最近激活 → 存储根默认工作区；
  - CLI 后端同样可被协作（消息经 JSONL 帧注入或新建运行，结果投回发起方会话）。
- **协作语义注入**：工具使用规范与投递正文说明「成员之间可互相发送协作消息，对方完成后回复自动送回发起会话」——各成员后端会话可见同一工具集，构成集群协同基础；投递正文「来自」修正为真实发起方（此前误写为接收方自身）。

### 0.2.7 追加（用户裁定：管理页可查看协助过程，暂不需要一键跳转）
- **事件自动标注发起会话**：集群协作运行的全部事件（run_started / completed / failed / 中断等）payload 自动携带 `origin_session`，无需逐调用点传参；
- **管理页协作观测**：成员详情「最近事件」对协作事件显示「来自 成员名」标注；页面底部新增「协作时间线」——跨成员关键事件按时间正序聚合为「发起方 → 目标成员：动作（摘要）」，主会话发起显示「主会话」；快照事件量 50 → 200 条。

### 0.2.9 追加（用户裁定：特性与记忆具备成长进化能力）

- **经验自主沉淀**：任务/消息投递正文附带成长引导——成员完成工作后把可复用经验（成功做法、踩坑、用户偏好，一行一条结论式）经 `append_agent_memory` 写入 `memory/lessons.md`，跨任务累积；
- **记忆注入按价值配额**：运行注入快照分文件配额（lessons 优先并给予最大份额，其次 notes，任务流水与其余文件填充剩余），避免低价值流水挤掉高价值经验，总量上限不变；
- **指令受控成长**：新增 AI 工具 `append_agent_instructions`——向成员长期指令**追加**新学到的稳定规则（带日期分段，不可覆盖既有内容；单次与总量设上限，超限提示整理）；权限规则：主会话可操作任意成员，成员后端会话只能操作自己（防止成员互改人格）。

## 关键决策

| 决策点 | 结论 | 依据 |
| --- | --- | --- |
| 插件形态 | schema v2：manifest 声明工具 + extension.tab + resident Rust sidecar，工具直连 sidecar | terminal 同款成熟形态 |
| 首版运行后端 | CLI Adapter（JSONL 非交互协议子进程，managed 进程组）；0.2.0 追加天工会话后端（WASM turn 钩子回报完成） | issue 接入优先级第 2 层；同时是阶段三 Claude Code/Codex 的地基 |
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
