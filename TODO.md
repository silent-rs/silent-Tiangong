# TODO - 天工当前开发任务

> 最后更新：2026-05-03
> 当前主线：Memory 系统缺口收口
> 参考：`PLAN.md`、`docs/requirements.md`、`docs/memory-system/12-缺口清单与专用MemoryLLM.md`

---

## 当前结论

接下来推进 Memory 系统缺口收口，覆盖专用 Memory LLM 配置、入口运行时统一、模型路径验证、检索质量补齐和生命周期完善。

能力边界：

- Memory 内部所有文本生成任务必须使用专用 `memory` LLM，不再复用主 `chat` 模型。
- 未配置 `memory` routing 时降级为规则策略，禁止静默回退到主模型。
- 所有入口统一通过 `start_or_connect()` 获取 Memory，确保多进程共享同一 workspace leader。
- Workspace Index 首期仅覆盖最小文件树索引和 Rust 符号索引。

---

## P0 - 入口运行时统一

- [ ] Core 启动入口从直接调用 `start_with_options()` 切换为 `start_or_connect()`。
- [ ] CLI/GUI/Server 主入口默认使用 `start_or_connect()` 获取 Memory。
- [ ] 同一 workspace 同时启动多个入口时只有一个 leader，其余入口使用 remote handle。
- [ ] leader 退出后 follower 自动接替，接替前后写入/召回都可用。
- [ ] 验证多进程并发场景（GUI + CLI + Server）下 Memory 行为正确。

## P0 - 专用 Memory LLM 配置

- [ ] `ModelCapability` 增加 `memory`。
- [ ] `models.json` 的 capability 列表和 routing 语义正式纳入 `memory`。
- [ ] `CoreConfig::to_memory_options()` 只从 `routing.memory` 读取文本模型。
- [ ] EpisodeWriter 结构化提取使用专用 Memory LLM。
- [ ] Recall anchor 规划使用专用 Memory LLM。
- [ ] Deep Recall 裁决使用专用 Memory LLM。
- [ ] Recall 结果整理使用专用 Memory LLM。
- [ ] Meso Entity/Decision 提炼使用专用 Memory LLM。
- [ ] 未配置 `memory` 时，上述步骤全部走规则 fallback。
- [ ] 日志明确提示 Memory LLM 未配置，不误报为普通模型失败。
- [ ] 禁止未配置 `memory` 时静默复用 `chat` 主模型或旧 `lite` 模型。

## P1 - 真实模型路径验证

- [ ] 增加可手动运行的 Memory LLM smoke test。
- [ ] 固定 5-10 个历史指代样例，验证输出只包含增量记忆。
- [ ] 记录 Memory LLM token 使用和延迟，确认不拖慢主对话链路。
- [ ] Deep Recall 真实效果通过跨会话、跨产物、跨 Entity/Decision 固定场景评测。

## P1 - 混合检索质量

- [ ] 默认测试链覆盖无外部服务的 embedded vector mock 或 deterministic provider。
- [ ] ignored 的真实 embedding 测试保留为手动验证，补充运行说明。
- [ ] 建立小型 recall benchmark，比较 BM25-only 与 hybrid 命中率。
- [ ] Qdrant 后端补齐归档删除同步和服务配置说明。

## P1 - Meta 与生命周期

- [ ] 归档时 SQLite、Tantivy、embedded vector、Qdrant 状态一致。
- [ ] 已归档节点从向量索引删除能力补齐（含外部 Qdrant delete）。
- [ ] 文件路径和媒体产物基本可达性检查。
- [ ] 过时检测覆盖文件删除、路径失效、项目归档、产物 URL 过期。
- [ ] Meta 执行结果进入可观测日志和指标。

## P2 - Workspace Index

- [ ] 最小文件树索引可生成、可查询、可按 workspace 隔离。
- [ ] Rust 符号索引至少覆盖 `mod/fn/struct/enum/trait`。
- [ ] Recall 可结合 Memory 与 workspace index 返回线索。
- [ ] 文件变更后的增量更新策略落地。

---

## 推荐执行顺序

1. 先实现专用 `memory` capability 与配置转换，阻断继续复用主模型。
2. 将 Core Memory 启动入口切换为 `start_or_connect()`，让 IPC/election 真正进入主链路。
3. 补真实 Memory LLM smoke test 和固定回忆样例集。
4. 补 embedded vector 的可重复测试，再处理 Qdrant 删除和服务化配置。
5. 最后推进 Meta 完整生命周期与 Workspace Index。

---

## 文档同步要求

后续实现上述任一缺口时，需要同步更新：

- `docs/requirements.md`
- `docs/memory-system/README.md`
- `docs/memory-system/09-分阶段落地路径.md`
- `docs/memory-system/11-当前开发状态与接手指南.md`
- `docs/memory-system/12-缺口清单与专用MemoryLLM.md`
