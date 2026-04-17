# Memory Phase A Review Round 2

- Review 时间：2026-04-17
- Review 提交：`d20513e`
- Review 范围：修复上一轮 review 后的 `memory-system` 相关提交

## 结论

本轮提交已经修复上一轮指出的 Tantivy 双 writer、`scope_id` 丢失和 Server 未接入 Memory 的问题；`cargo check --workspace`、`cargo clippy --workspace --all-targets --tests --benches -- -D warnings`、`cargo test -p tiangong-memory` 也均已通过。

但当前实现里仍有 3 个需要继续处理的问题，其中前 2 个属于运行时设计缺陷。

## Findings

### [P1] 全局 Memory 单例会把后续工作区错误绑定到首个 workspace

- 位置：[crates/tiangong-memory/src/lib.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-memory/src/lib.rs:40)
- 说明：
  `ensure_started()` 在首次启动后会直接返回已有全局 handle，并明确忽略后续传入的 `workspace_id`。但 `MemoryStore` 会把这个启动时的 `workspace_id` 固化到后续 Episode 落库路径里，所以同一进程里如果后面再打开其他 workspace，记忆仍会继续写进第一个 workspace 的 scope。这在 Server/GUI 这种长生命周期、多上下文进程里会导致跨工作区记忆串写。

### [P2] `ensure_started()` 并发调用时可能启动多个孤儿 Memory Actor

- 位置：[crates/tiangong-memory/src/lib.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-memory/src/lib.rs:44)
- 说明：
  这里先 `GLOBAL_HANDLE.get()`，再在未加锁的情况下执行 `start(workspace_id)`。如果两个线程同时第一次调用，都可能看到未初始化状态并各自启动一个 actor 线程；最后 `OnceLock` 只保留其中一个 handle，另一个 actor 会继续运行但再也拿不到引用，白白占住 sqlite/tantivy 资源。文档注释写的是“幂等，多次调用安全”，但当前实现并不满足并发安全。

### [P3] 第一轮 review 报告已经与当前代码状态不一致

- 位置：[docs/memory-system/review/2026-04-17-memory-phase-a-review.md](/Users/hubertshelley/Documents/silent/tiangong/docs/memory-system/review/2026-04-17-memory-phase-a-review.md:4)
- 说明：
  这份报告仍然指向旧提交 `27da4c6`，并把当前提交中已经修复的 Tantivy 双 writer、`scope_id` 丢失、Server 未接入 handle 继续列为现存问题。把这类已过期结论继续保留在同一目录里，会误导后续排查和评审。至少应在旧报告顶部注明“已被后续提交部分修复”，或者补一条索引说明第二轮报告位置。

## 验证

- `timeout 180 cargo check --workspace`
- `timeout 240 cargo clippy --workspace --all-targets --tests --benches -- -D warnings`
- `timeout 180 cargo test -p tiangong-memory`

## 建议

1. 不要把 `workspace_id` 固化到进程级全局单例里，改为按 workspace 隔离 actor，或让写入命令显式携带 workspace 上下文。
2. 让 `ensure_started()` 的初始化过程具备真正的并发安全，避免启动多个无引用 actor。
3. 在第一轮报告中补充“已过期/已被后续提交修复”的标记，或者增加统一索引页说明两轮 review 的先后关系。
