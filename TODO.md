# TODO - 天工全栈平台重构任务清单

> 最后更新：2026-04-20
> 当前主线 RFC：`docs/rfc/0004-full-stack-agent-platform.md`
> 参考：`PLAN.md`、`docs/requirements.md`

---

## 已完成阶段摘要

- `Phase 2`：Skill 管理 MVP 已完成，包含安装、启停、卸载、锁文件、托管 MCP 清理、事务回滚与审计。
- `Phase 3`：Workspace 拆分与核心抽离已完成，`tiangong-core` / `tiangong-cli` / `tiangong-entry` / `src-tauri` 已稳定运行。
- `Phase 4`：Server 模式已完成，REST、WebSocket、Token 认证、后台运行与停止命令已落地。
- `Phase 5`：Gateway 与 EventBus 已完成，统一消息模型和消息路由已投入使用。
- `Phase 6`：Connector 框架已完成，Webhook、Telegram、Discord、Lark 已接入。
- `Phase 7`：多媒体框架已完成，图片生成、STT、TTS 与媒体任务模型已接入主链路。
- `Phase 8`：生产化增强已完成，日志分级、错误恢复、配置脱敏、配置热重载已上线。
- `Phase 9`：模型配置重构与多媒体集成已完成，`ModelsConfig` 三层架构与多媒体 routing 已稳定。
- `Phase 10`：GUI/CLI 友好交互改造已完成，解释文本和工具调用支持实时展示。
- `Phase 11`：运行时基础设施补全已完成，统一任务模型、查询编排、恢复、权限和成本治理已落地。
- `Phase 12`：事件驱动循环运行时已完成，EventLoopRunner 已替代旧的主执行链。
- `Phase 13`：`TiangongCore` 纯粹化与统一类型已完成，CLI/GUI/Server 已统一到核心流事件接口。
- `Phase 13.5`：LLM 请求容错已完成，重试、错误展示、审批流程和工具展示优化已接入。
- `Phase 14`：CoreConfig 配置注入已完成，CLI/GUI/Server 的配置同步与即时生效链路已打通。
- `Phase 15`：LLM 协议抽象与 Anthropic 支持已完成，统一 Provider 抽象、Anthropic transport、协议配置透传与兼容性处理已接入。
- `Phase 16`：架构收口与远程能力补齐已完成，统一入口、Server 信任语义、远程成本可见性和历史文档收敛已落地。

---

## Phase 17：多媒体结果语义收敛 — **进行中**

> 对照 `docs/media-capability-architecture-adjustment.md` 继续推进

### A. 媒体结果模型

- [x] 为统一消息模型增加结构化媒体结果字段
- [x] 定义图片结果最小可用结构（URL / MIME / 来源能力 / 标题）
- [x] 保持文本内容与媒体内容并存，避免强制回退到 Markdown 文本

### B. 本地 GUI 图片链路

- [x] 将本地图片生成最终结果改为结构化媒体消息，而不是工具日志文本
- [x] 区分最终结果图片与中间过程图片，避免一律提升为最终 assistant 回复
- [x] 前端基于结构化媒体消息渲染图片，保留旧 Markdown 图片渲染作为兼容路径

### C. 后续扩展口

- [ ] 为视频结果预留相同的结构化媒体语义
- [ ] 约束 MCP 只作为媒体后端适配来源，不直接定义上层媒体消息语义

---

## Phase 18：Memory 系统 — **进行中**

> 对照 `docs/memory-system/` 目录中的设计文档推进

### 当前剩余未完成功能总览

- [x] 补齐 `db/sqlite.rs` 对 `Entity / Decision` 的完整 CRUD
- [ ] 将 `writer.rs` 升级为调用 lite 模型提取 Episode 摘要、结果状态、工具调用和重要度
- [x] 将当前 TCP IPC 原语真正接入 `MemoryHandle` 的 local/remote 双态
- [x] 补齐 `election/` 的 Follower 监控、leader 切换与自动接替
- [ ] 为 GUI / 桌面入口补齐显式 Memory 启动与 Handle 注入
- [ ] 设计并实现按 workspace 管理的 runtime / handle registry，避免后续多工作区冲突
- [x] 实现 `search/embedding.rs`，封装 `EmbeddingProvider` 的 memory 侧调用
- [x] 打通 Episode -> Embedding -> 内置向量索引 upsert 的主写链路
- [x] 增加内置向量索引默认启用与 Qdrant external 兼容降级策略
- [ ] 实现真正的 `RecallAnchors` 提取，而不是只使用原始 query
- [ ] 完成 `LoadDepth2` 的定向展开能力
- [ ] 将 `process_meso()` 从关键词统计升级为真实的 `Entity / Decision` 提炼
- [x] 为 Memory 增加更多集成测试，覆盖多进程 IPC / leader 切换 / embedded 混合检索主链路
- [ ] 为 Memory 增加 external Qdrant 专项集成测试（可选，需外部服务）

### Memory Phase A：Injection 层 + 独立 crate 骨架 + SQLite 元数据库 ✅

- [x] 创建 `crates/tiangong-memory` 独立 crate
- [x] 实现基础类型（`types.rs`）：MemoryNode, Episode, Entity, Decision 等
- [x] 实现 MemoryActor + MemoryHandle（Actor 消息协议 + 客户端句柄）
- [x] 实现三级 Injection 读取（`injection.rs`）：Profile / Workspace / Session agent.md
- [x] 实现 SQLite 加密元数据库（`db/`）：sqlcipher + WAL + 建表
- [x] 实现 Leader 选举骨架（`election/mod.rs`）
- [x] 添加 `init_blocking()` 同步初始化函数
- [x] 将 `tiangong-memory` 加入 workspace members
- [x] `tiangong-core` 添加 `tiangong-memory` 依赖
- [x] 修改 `build_user_context()` 调用 `tiangong_memory::load_injection_sync`
- [x] CLI / Server 入口添加 Memory 初始化调用
- [x] `context/memory.rs` 标记废弃注释
- [x] `cargo clippy --workspace` 全量通过

### Memory Phase B：Episode 写入 + IPC + Tantivy 全文检索

- [x] `tiangong-memory` 引入 `tantivy = "0.22"` 依赖
- [x] 实现 `search/tantivy.rs`：Tantivy Schema 定义、文档索引、BM25 查询
- [x] 扩展 `store.rs`：协调 SQLite + Tantivy 双层写入
- [x] 扩展 `db/sqlite.rs`：Episode/Entity/Decision 完整 CRUD
- [ ] 实现 `writer.rs`：EpisodeWriter（调用 lite 模型提取摘要）
- [x] 实现 `rumination.rs`：MicroRumination（Episode 写入部分）
- [x] 扩展 `actor.rs`：处理 WriteEpisode / RunMicroRumination 命令
- [x] 实现 `ipc/`：TCP loopback IPC 服务端/客户端骨架（动态端口 + endpoint 文件发现 + token 鉴权）
- [x] 扩展 `election/`：Follower 监控与自动接替
- [x] 各入口集成完整 Memory Actor 启动（CLI / Server 已显式启动并注入 Handle）
- [x] runtime 集成点：turn 完成后通过 Handle 发送反刍命令
- [x] 为 `tiangong-memory` 增加独立集成测试（runtime / ipc 主链路）

### Memory Phase C：内置向量检索 + 双引擎召回 + Recall 注入

- [x] 引入 `qdrant-client = "1"` 依赖
- [x] 实现 `search/vector.rs`：内置 SQLite flat 向量索引与 `VectorIndex` 抽象
- [x] 实现 `search/qdrant.rs`：collection 管理、point upsert、语义查询（作为 external backend 兼容路径）
- [x] 实现 `search/embedding.rs`：封装 EmbeddingProvider 调用
- [x] 实现 `search/reranker.rs`：分数归一化、时间衰减、重要度加权
- [x] 实现 `search/mod.rs`：双引擎召回统一入口（BM25 + Vector）
- [x] 实现 `recall.rs`：RecallAnchors 提取 + 双引擎召回 + 定向展开（Depth2 未完成）
- [x] 打通 `Episode -> Embedding -> EmbeddedFlatVectorIndex -> Hybrid Recall` 主链路
- [ ] 扩展 `rumination.rs`：MesoRumination（Entity/Decision 提取）
- [x] 修改 `prompt/assembler.rs`：Recall 结果注入到 Prompt
