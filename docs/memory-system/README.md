# 天工 Memory 系统设计

## 设计目标

天工 Memory 系统的核心目标不是"存更多"，而是：

- 低成本粗召回，按意图逐步深入
- 多层细节展开，上下文预算可控
- 可审计、可维护、可迁移
- 与现有 Prompt 装配和事件循环架构无缝融合

核心理念：

> Memory 不应一次性注入，而应像人类回忆一样逐步展开。

## 文档目录

| 文档 | 说明 |
|------|------|
| [01-记忆分层设计.md](01-记忆分层设计.md) | 记忆分层设计（7 层模型） |
| [02-注入层设计.md](02-注入层设计.md) | 注入层设计（Profile/Workspace/Session 三级注入） |
| [03-渐进式回忆.md](03-渐进式回忆.md) | 渐进式回忆流程 |
| [04-工作区文件索引.md](04-工作区文件索引.md) | 工作区文件索引 |
| [05-反刍与反思层.md](05-反刍与反思层.md) | 反刍与反思层 |
| [06-模块划分与代码归属.md](06-模块划分与代码归属.md) | 模块划分与代码归属 |
| [07-持久化格式与技术选型.md](07-持久化格式与技术选型.md) | 持久化格式与技术选型 |
| [08-并发安全策略.md](08-并发安全策略.md) | 并发安全策略 |
| [09-分阶段落地路径.md](09-分阶段落地路径.md) | 分阶段落地路径 |
| [10-预算控制与性能策略.md](10-预算控制与性能策略.md) | 预算控制与性能策略 |

## 与现有架构的关系

Memory 系统作为独立 crate `tiangong-memory` 实现，采用 **Actor 模型**独立运行，外部通过 `MemoryHandle` 消息通讯访问：

```
crates/
  tiangong-memory/           ← 【独立 crate】Memory 基础设施
    src/
      lib.rs                 ← crate 入口，导出公共 API
      command.rs             ← MemoryCommand 消息协议
      handle.rs              ← MemoryHandle（客户端句柄，支持自动重连）
      actor.rs               ← MemoryActor（独立 tokio task 运行时）
      store.rs               ← MemoryStore（Actor 内部私有）
      injection.rs           ← Injection 文件读写
      recall.rs              ← Progressive Recall
      writer.rs              ← Episode/Decision 写入
      rumination.rs          ← 反刍（后期）
      ipc/                   ← 跨进程 IPC 服务端/客户端
      election/              ← Leader 选举与迁移

  tiangong-core/src/         ← 智能体核心逻辑（依赖 tiangong-memory）
    prompt/
      assembler.rs           ← 通过 MemoryHandle 查询 Injection / Recall
      sections.rs            ← build_user_context() 通过 Handle 异步查询
    context/
      organizer.rs           ← 压缩完成后通过 Handle 发送反刍命令
    workspace_index/         ← 【新增模块】
      mod.rs
      file_tree.rs
      symbol_index.rs
```

**依赖方向**：
```
tiangong-cli、tiangong-server、src-tauri
  └─→ tiangong-memory   ← 启动时初始化 MemoryHandle
  └─→ tiangong-core     ← 将 Handle 传入 core
        └─→ tiangong-memory  ← 类型引用和 Handle 调用
```

**通讯模型**：
```
调用方 ──(mpsc channel / IPC)──→ MemoryActor ──→ 磁盘读写
       ←(oneshot channel / IPC)── 查询响应
```

**所有运行模式都必须接入 Memory**，包括 GUI、TUI、Server、CLI，不允许任何模式跳过。

## 磁盘目录结构

```
~/.tiangong/
  memory/
    profile/
      agent.md
    workspaces/
      <workspace_id>/
        agent.md
        entities/
        episodes/
        decisions/
        evidence/
    sessions/
      <session_id>/
        agent.md
  workspace-index/
    <workspace_id>/
      file-tree.json
      symbols.json
```

## 分阶段交付

| 阶段 | 内容 | 代码变更量 | 详见 |
|------|------|-----------|------|
| Phase A | Injection 层 + 独立 crate 骨架 | ~300 行 | [09-分阶段落地路径.md](09-分阶段落地路径.md) |
| Phase B | Episodic Memory 写入 + IPC | ~500 行 | [09-分阶段落地路径.md](09-分阶段落地路径.md) |
| Phase C | Progressive Recall | ~500 行 | [09-分阶段落地路径.md](09-分阶段落地路径.md) |
| Phase D | Rumination + Workspace Index | ~800 行 | [09-分阶段落地路径.md](09-分阶段落地路径.md) |
