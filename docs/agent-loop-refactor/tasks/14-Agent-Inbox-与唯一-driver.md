# 任务 14：Agent Inbox 与唯一 driver

> 需求：[../requirements.md](../requirements.md) §3.2/3.3 ｜设计：[../design.md](../design.md) §3~7
>
> 状态：**已完成**｜对应方案阶段 2（建立 Agent 自有 Inbox 和唯一 Driver）。

## 目标

1. 建立 Agent 自有 `next_turn`（followup FIFO）与 `next_step`（inject 积压）队列，输入语义显式化（ALR-101~106）。
2. Agent 对外只有 `Idle | Running`；同一 Agent 至多一个 driver，由同一 driver 连续处理后续 turn（ALR-001/004/104）。
3. 取消收敛窗口用唤醒锁存（`Phase::Idle` + `Notify` permit），不再创建补偿后台任务（ALR-105）。
4. 下一 turn 真正开始时从最新磁盘 Session 与最新配置构建上下文，不持有旧快照（ALR-201/204）。
5. 关闭先停止接收，再取消当前轮并等待 driver 收敛；已确认消息持久化为可恢复状态，不静默丢弃、不偷偷执行（ALR-202/206）。
6. 删除临时 `NEXT_TURN_QUEUE` 与每条消息一次性后台任务。

## 实施结果

### 模块归属

- 新增 `react/inbox.rs`：Agent 调度（Inbox、唤醒锁存、注册表、关闭）。实施中途按 Review 意见从 `shared_runtime.rs` 迁出——调度是 Agent 层领域逻辑，不属于进程级 runtime 基础设施。
- `shared_runtime.rs` 回归纯 runtime（仅共享 tokio runtime 获取），约 35 行。
- `core/mod.rs`：`TurnSpawner`（每轮构建材料快照）+ `drive_agent` 唯一 driver 主循环 + `deliver` 语义映射。

### 输入语义（ALR-106）

`MessageInput::UserMessage` 新增 `delivery: MessageDelivery` 字段（`Followup` 默认 / `Steer`），便捷构造器默认 Followup；现有宿主（GUI `prepared_with_id`、agent-team、server 转发）无需改动源码。

| 输入 | Running 时 | Idle 时 |
| --- | --- | --- |
| followup | 进入 `next_turn` FIFO，当前 turn 结束后开新 turn | 入队并唤醒 driver |
| steer | 投递 `InjectUserMessage` 到当前轮（封口时回退 `next_turn`，不丢） | 等同 followup |
| inject（Tool） | 投递 `InjectTool` 到当前轮 | 积压 `next_step`，不唤醒 |
| Cancel / Approval / 配置 | `send_command` 到当前轮 | 无操作 / 明确拒绝 |

手动压缩与重置上下文改为 Inbox 控制输入（`ManualCompression` / `ResetContext`），由 driver 在空闲时串行执行，与后续用户消息天然排序，不再独立 spawn。

### driver 主循环

```text
loop {
    关闭判定：不再接受 → 持久化 Inbox 未处理用户消息（可恢复）→ 退出
    take_next_turn()：
        UserMessage → 最新 Session 构建 → 保存+确认 → run_turn → end_turn
        ManualCompression / ResetContext → 对应维护活动
    队列空 → try_park（临界区复查 next_turn）→ notified() 挂起等待唤醒
}
```

- 唤醒锁存：`try_park` 与投递方唤醒判定互斥，`Notify` permit 覆盖 park 之后、await 之前的窗口（ALR-105）。
- `busy`/`parked` 双标志合并为单一 `Phase { Idle, Running }`。
- 修复实施中发现的两个缺陷：`try_park` 曾因检查 `next_step` 导致 turn 间隙收到 inject 后自旋（inject 语义本就是等待下次活动）；`ensure_agent_session` 曾在补建 driver 时覆盖已有 Inbox 载体丢失 `next_step` 积压。

### 删除对象

- `shared_runtime.rs`：`NEXT_TURN_QUEUE` 全部函数（`push/has/clear/requeue/drain_next_turn`）、`TURN_TASKS` 旧注册表、`spawn_turn`、`cancel_and_join`、`dummy_context`。
- `core/mod.rs`：`queue_next_turn_and_auto_start`（每消息后台任务+轮询）、`consume_next_turn_and_start`、`save_and_confirm_pending`、`spawn_next_turn`。
- core-manager `delete_session` 的 `cancel_and_join` 改用 `react::inbox::shutdown_agent`（取消 + 等 driver 收敛，语义一致且多了排空持久化）。

### 资源模型说明

driver 是长期任务：第一次投递时启动，空闲挂起在 `notified().await`（不占线程、不耗 CPU，仅保留 future 状态与 Inbox 的少量内存），关闭或 Core Drop 时退出。`TiangongCore` 创建时仍为零任务。宿主不关不 drop 的 Core 会积累挂起 driver——`Drop` 已做 `detach_shutdown` 非阻塞收敛，正常宿主生命周期无泄漏。

## 测试

- 任务 13 的 4 项调度失败用例移除 `#[ignore]` 后全部转绿：followup FIFO 各成独立 turn、封口交接读最新 Session、关闭不静默丢已接受消息、空闲 inject 接受且不唤醒。
- 旧 `sealing_window_message_auto_starts_next_turn_with_single_confirmation` 改写为 `running_turn_message_auto_starts_next_turn_with_single_confirmation`（同一 driver 自动交接 + 单次确认）。
- 新增 `inbox_delivers_turns_in_fifo_with_single_driver`（Inbox 单元：FIFO、park/唤醒互斥、next_step 积压不阻止挂起、关闭后拒绝）。
- `next_turn_queue_round_trip` / `requeue_next_turn_front_preserves_order` / `registry_level_sealing_gates_send_command` 随旧队列删除（按任务 13 分类）。
- react 层 4 项工具义务失败用例保持失败形态（任务 15 安全网未受影响）。

## 验证记录

- `cargo fmt -- --check`：通过。
- `cargo clippy --workspace --all-targets --tests --benches -- -D warnings`：通过。
- `cargo check --workspace`：通过（含 core-manager、server、src-tauri 在内的全部下游）。
- `cargo test -p tiangong-core`：112 通过、4 ignored（react 工具义务，任务 15 启用）、0 失败。
- `cargo test -p tiangong-plugin-agent-team`：10 通过、1 ignored。
- `cargo test -p tiangong-plugin-browser`：32 通过。
- `cargo test -p tiangong-core contract_tests -- --ignored`：4 项工具义务用例全部保持失败（`Success` 虚假完成、工具未执行等），失败形态与任务 13 记录一致。

## 行为变化说明

运行中的用户消息默认从「注入当前轮并重启」改为「排队为下一 turn」（followup）。需要旧行为的调用方可显式使用 `MessageDelivery::Steer`（映射到原注入命令路径）。GUI/CLI/Server 源码无需修改，但用户可感知的交互节奏变化需在交付说明中标注。

## 遗留与后续

- steer 在「下一 step 生效」的精确语义（而非当前轮重启）随任务 15 的 Loop 收敛处理。
- 任务 15：启用 react 层 4 项工具义务用例，删除 Summary/ForceFinal/continuation。
