# TODO - 天工当前开发任务

> 最后更新：2026-04-22
> 当前主线：Phase 18 Memory 系统收口
> 参考：`PLAN.md`、`docs/requirements.md`、`docs/memory-system/`

---

## 当前结论

Memory 主链路已经可用，但还不能视为完全收口。当前最重要的差距不是“能不能写入和回忆”，而是回忆策略、Meso 提炼质量、运行时生命周期、配置热更新和外部向量后端验证。

已完成能力只在本节压缩记录，后续 TODO 只保留真实开发差距。

---

## 已完成能力摘要

- 平台基础：Workspace 多 crate、Core/CLI/GUI/Server 拆分、Server 模式、Connector、EventBus、多媒体框架、CoreConfig 注入、LLM 协议抽象、Anthropic 支持、远程信任语义和成本可见性已完成。
- Phase 17 多媒体：图片结果已进入结构化消息链路，本地 GUI 可渲染结构化图片结果，旧 Markdown 图片仍保留兼容。
- Phase 18 Memory 基础：`tiangong-memory` 独立 crate、SQLite 元数据库、Injection、Actor/Handle、TCP IPC、Leader/Follower、workspace 显式写入上下文已完成。
- Phase 18 写入/检索：Episode 写入、Tantivy BM25、内置 SQLite flat 向量索引、Qdrant 兼容路径、BM25+Vector 混合召回、Depth2 展开已完成。
- Phase 18 Tool 化回忆：Core 已移除 turn 前自动 Recall，改为主模型按需调用 `recall_memory`；Memory 内部负责规划、召回、展开和去重整理。
- Phase 18 产物记忆：媒体 URL、文件路径、工具结果摘要已写入 Episode，可支持“刚刚生成的图片/文件”等回忆。
- Phase 18 模型能力收口：Memory 文本生成与 embedding 配置复用 `tiangong-llm`，不再在 Memory 内重复实现模型配置和协议适配。
- Phase 18 Meso 初版：规则版 Entity/Decision 提炼已接入 SQLite/Tantivy，并更新 Workspace Injection。
- Phase 18 测试：Memory 已覆盖 runtime、IPC、leader failover、embedded 混合检索、artifact-only 写入、增量回忆去重、Meso Entity/Decision。

---

## P0 - 必须优先收口

### 1. 抽出稳定的 `RecallAnchorExtractor`

- [x] 新增 `recall_anchor.rs`，提供统一的 `RecallAnchorExtractor` 入口。
- [x] 将 `recall_context` 中的 LLM plan 逻辑迁移到 `RecallAnchorExtractor`。
- [x] 将 `recall_context` 中的规则 fallback 逻辑迁移到 `RecallAnchorExtractor`。
- [x] 让 LLM 规划和规则 fallback 都输出同一个 `RecallAnchors` 结构。
- [x] 规则 fallback 覆盖历史指代、文件路径、URL、工具名、代码符号、媒体产物和用户显式关键词。
- [x] 明确 `SearchStrategy::Skip` 的入口语义，避免普通闲聊触发无意义检索。
- [x] 为 anchor 提取增加单元测试：历史指代、精确文件路径、媒体 URL、普通闲聊、空输入。

### 2. 修正 Meso Entity/Decision 的幂等与质量问题

- [x] 为 Entity 生成稳定 key，按 `(workspace_id, entity_type, name)` 去重更新。
- [x] 为 Decision 生成稳定 dedupe key，避免同一 Episode 多次生成重复 Decision。
- [x] 增加 LLM 版 Meso 提炼器，复用 `tiangong-llm` 输出结构化 Entity/Decision。
- [x] 为 LLM Meso 输出增加严格 JSON 解析、字段校验和错误 fallback。
- [x] 保留当前规则版 Meso 作为 LLM 失败时的 fallback。
- [x] 增加集成测试验证重复运行 Meso 不会重复膨胀 Entity/Decision 数量。
- [x] 增加测试验证 LLM Meso 失败时仍能回退到规则版。

### 3. Memory runtime / handle registry 生命周期收口

- [x] 为 registry entry 记录 workspace_id、配置摘要或 generation、创建时间、最后使用时间。
- [x] 定义配置 generation 变化时的处理策略：memory 相关摘要变化时复用旧 handle 并标记待重启。
- [x] 保证 `TiangongCore::into_session` / Drop 不误关共享 MemoryHandle。
- [x] 增加应用退出路径的统一 Memory shutdown 能力。
- [x] 增加测试验证两个 workspace 使用不同 handle。
- [x] 增加测试验证两个 workspace 的 Episode 不会串写 scope_id。

---

## P1 - 主链路质量增强

### 4. 更新 Memory 接手指南

- [ ] 更新 `docs/memory-system/11-当前开发状态与接手指南.md` 到当前代码状态。
- [ ] 删除或标注旧结论：Meso 只是关键词统计、RecallAnchors 未规划、Server 未接入等。
- [ ] 补充当前可用链路：Micro、Meso、Tool 化回忆、Depth2、混合检索、产物记忆。
- [ ] 补充测试脚本和观察日志命令。
- [ ] 补充当前已知限制和下一步任务。

### 5. external Qdrant 专项集成测试

- [ ] 新增 Qdrant ignored 集成测试。
- [ ] 使用 `TIANGONG_MEMORY_QDRANT_TEST=1` 显式启用测试。
- [ ] 覆盖 Qdrant collection 创建。
- [ ] 覆盖 Qdrant point upsert。
- [ ] 覆盖 Qdrant semantic search。
- [ ] 未设置环境变量或服务不可用时跳过，不影响默认 CI。

### 6. Memory 配置热更新

- [ ] 定义 Memory 配置摘要，覆盖 model、embedding、dimension、vector_mode。
- [ ] registry 根据配置摘要判断是否复用旧 handle。
- [ ] 明确哪些配置变更需要重启 Memory actor。
- [ ] 明确哪些配置变更可以原地更新。
- [ ] embedding 维度变化时拒绝复用旧向量索引。
- [ ] embedding/vector 配置不兼容时输出 warning 并降级为 BM25-only。
- [ ] 增加配置变更相关单元测试或集成测试。

### 7. Recall 输出预算与去重策略继续收口

- [ ] 为 `MemoryRecallResponse` 增加统一输出预算策略。
- [ ] 至少按字符数或估算 token 限制 recall 输出长度。
- [ ] 同一 node_id 不重复输出。
- [ ] 同一 URL 不重复输出。
- [ ] 同一路径不重复输出。
- [ ] 同一工具结果摘要不重复输出。
- [ ] 当前上下文已有内容不重复输出。
- [ ] 增加长 Episode 裁剪测试。
- [ ] 增加重复 URL、重复 path、重复当前上下文测试。

---

## P2 - 后续增强

### 8. Memory 观测与调试能力

- [ ] tracing 日志输出 recall query。
- [ ] tracing 日志输出 recall strategy。
- [ ] tracing 日志输出 hit count。
- [ ] tracing 日志输出 backend：BM25、embedded vector、Qdrant。
- [ ] tracing 日志输出 used_llm 和 fallback reason。
- [ ] debug 日志可观察 Episode 写入。
- [ ] debug 日志可观察 vector upsert。
- [ ] debug 日志可观察 Meso Entity/Decision 提炼数量。

### 9. Phase 17 多媒体尾项

- [ ] 为视频结果补齐与图片一致的结构化消息字段。
- [ ] GUI 支持渲染结构化视频结果。
- [ ] Connector 支持发送结构化视频结果。
- [ ] 约束 MCP 只作为媒体后端来源之一。
- [ ] 防止多媒体结果重新退化为工具文本。

### 10. requirements 文档补齐 Memory 需求

- [ ] 在 `docs/requirements.md` 增加 Memory 系统 Must/Should 要求。
- [ ] 明确 Memory 功能必须可关闭。
- [ ] 明确 Memory 必须保持独立 crate。
- [ ] 明确 Memory 必须复用 `tiangong-llm`。
- [ ] 明确 Memory 必须支持按需回忆。
- [ ] 明确 Memory 必须支持 workspace 隔离。
- [ ] 明确 Memory 必须支持产物记忆。
- [ ] 明确 Memory 必须具备独立集成测试。

---

## 当前推荐执行顺序

1. 收口 Memory runtime / handle registry 生命周期。
2. 更新 Memory 接手指南到当前代码状态。
3. 补 external Qdrant ignored test。
4. 处理 Memory 配置热更新。
5. 继续收口 Recall 输出预算与去重策略。
