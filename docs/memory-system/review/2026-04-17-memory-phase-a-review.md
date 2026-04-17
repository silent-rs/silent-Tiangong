# Memory Phase A Review

> **⚠️ 已过期** — 本报告所指的 3 个问题（Tantivy 双 writer、`scope_id` 丢失、Server 未接入 Memory Handle）已在后续提交中全部修复。
> 当前代码状态请参阅第二轮 Review 报告：[2026-04-17-memory-phase-a-review-round2.md](./2026-04-17-memory-phase-a-review-round2.md)

- Review 时间：2026-04-17
- Review 提交：`27da4c6`（**已被后续提交部分覆盖，结论不再适用**）
- Review 范围：本次 `memory-system` 相关提交

## 结论

本次提交可以通过 `cargo check --workspace` 和 `cargo clippy --workspace --all-targets --tests --benches -- -D warnings`，但从运行时行为看仍有 3 个需要优先处理的问题。

## Findings

### [P1] `MemoryStore::open()` 对同一 Tantivy 索引打开了两个 writer，Memory Actor 启动时存在直接失败风险

- 位置：[crates/tiangong-memory/src/store.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-memory/src/store.rs:33)
- 说明：
  `open()` 里先执行一次 `TantivyIndex::open(&base)?`，紧接着又执行第二次 `TantivyIndex::open(&base)?` 并把它交给 `RecallEngine`。而 `TantivyIndex::open()` 内部会创建 `IndexWriter`，同一索引目录通常只能持有一个 writer 锁。这样启动 Memory 时就可能因为第二次 writer 初始化拿不到锁而失败，导致 CLI/Server 虽然编译通过，但实际运行时 Memory Actor 无法启动。

### [P1] Episode 落库时把 `scope_id` 固定写成 `NULL`，工作区隔离信息被直接丢失

- 位置：[crates/tiangong-memory/src/db/sqlite.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-memory/src/db/sqlite.rs:45)
- 说明：
  `memory_nodes` 的写入 SQL 把 `scope_type` 固定为 `'workspace'`，但 `scope_id` 直接写成 `NULL`。上层 `episode_to_node()` 已经根据路径生成了 `workspace_id`，但这里完全没有入库，结果是所有 Episode 都退化成“无工作区归属”的记录。后续无论是按工作区回忆、工作区注入，还是跨项目隔离，都会拿不到正确的过滤条件。

### [P2] Server 入口只启动了 Memory Actor，但没有把 handle 接入 `ServerAppContext`

- 位置：[crates/tiangong-server/src/lib.rs](/Users/hubertshelley/Documents/silent/tiangong/crates/tiangong-server/src/lib.rs:32)
- 说明：
  `run_server()` 里已经启动了 Memory，并把结果保存在 `_memory_handle`，但紧接着注释也写明“TODO: 将 _memory_handle 传递给 ServerAppContext”。这意味着当前只有 CLI 路径真正把 handle 传进了 `TiangongCore`，Server 路径实际上还没有接入 recall / rumination / injection 的主链路。对外表现会是“Server 启动了 memory，但请求侧完全感知不到”。

## 验证

- `timeout 120 cargo check --workspace`
- `timeout 180 cargo clippy --workspace --all-targets --tests --benches -- -D warnings`

## 建议

1. 先拆掉 `MemoryStore` 内对同一索引的双 writer 打开方式，改成共享 reader / 延迟初始化 recall 视图。
2. 把 `workspace_id` 贯穿到 SQLite 写入层，至少保证 `memory_nodes.scope_id` 与 `scope_type='workspace'` 一致。
3. 为 Server 请求链路补齐 `MemoryHandle` 透传，否则 CLI 与 Server 的行为会持续分叉。
