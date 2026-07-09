# 浏览器 per-session 化 - 进度记录

> 本文件持续记录开发状态。任何一次继续开发，先读本文件恢复上下文。

## 当前状态

**阶段**：工程化规划完成，待开始 T1 编码。

**当前建议任务**：T1（per-session 状态注册表骨架）。

**当前阻塞**：无。

**开发分支**：`feature/trait-plugin-225`（已含 trait 迁移 + review 修复两个提交；per-session 化在此分支继续）。

## 关联文档

- 需求：`docs/browser/05-per-session-architecture.md`
- 总体设计：`docs/rfc/0016-browser-per-session.md`
- 任务 spec：`docs/browser/tasks/T1 ~ T8`
- 设计依据（能力下沉背景）：本分支前两个提交（`695fede8` trait 迁移、`a857fe1e` review 修复）

## 任务总览

| 任务 | 名称 | 状态 | 依赖 |
|------|------|------|------|
| T1 | per-session 状态注册表骨架 | 未开始 | 无 |
| T2 | BrowserManager 持有 registry | 未开始 | T1 |
| T3 | BrowserCommand 带 session_id + fetcher 注入 | 未开始 | T2 |
| T4 | handler 按 session_id 路由 | 未开始 | T2, T3 |
| T5 | 多 webview 并发 + 切换不销毁 | 未开始 | T2, T4 |
| T6 | watcher 与事件路由 session-aware | 未开始 | T3, T4, T5 |
| T7 | per-session 持久化与恢复 | 未开始 | T3, T6 |
| T8 | 端到端验证与清理 | 未开始 | T1-T7 |

## 任务依赖图

```
T1 (registry 骨架)
 └─ T2 (manager 持 registry)
     ├─ T3 (command 带 session_id)
     │   └─ T4 (handler 路由)
     │       └─ T5 (多 webview 并发)
     │           └─ T6 (watcher/事件 session-aware)
     │               └─ T7 (per-session 持久化)
     │                   └─ T8 (端到端验证)
```

关键路径：T1 → T2 → T3 → T4 → T5 → T6 → T7 → T8（线性，无并行空间——各任务共享 manager/handler 代码，不宜并行）。

## 里程碑记录

- **2026-07-09**：工程化规划完成。需求文档、RFC 0016、T1-T8 spec、本进度文件就绪。trait 迁移 + review 修复已提交（`695fede8`、`a857fe1e`）。

## 验证记录

| 任务 | 验证命令 | 结果 | 日期 |
|------|---------|------|------|
| trait 迁移 | cargo check --workspace --tests | 零 warning/error | 2026-07-09 |
| trait 迁移 | cargo test (core 67/browser 20/terminal 38/app-state 19) | 全过 | 2026-07-09 |
| review 修复 | cargo check --workspace --tests | 零 warning/error | 2026-07-09 |
| review 修复 | cargo test (core 67/browser 25/terminal 38) | 全过 | 2026-07-09 |

## 更新规则

每完成一个任务，更新：
1. 任务总览表的状态列。
2. 新增里程碑记录（日期 + 完成内容 + 提交 hash）。
3. 验证记录表（命令 + 结果）。
4. 当前建议任务指向下一个。
5. 若遇阻塞，填写当前阻塞 + 原因。

## 遗留问题

- 阶段 D（T5）多 webview 并发需 macOS 真实环境手动验证（命令行环境无法跑 GUI）。T5 完成后必须在真实环境验收。
- Core `Session.tabs` browser tab 字段保留兼容，后续单独清理（不在本次范围）。
