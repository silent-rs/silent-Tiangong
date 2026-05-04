# 12 - 缺口清单与专用 Memory LLM

> 更新日期：2026-05-03

本文档记录当前 Memory 系统相对设计目标的剩余缺口，并冻结模型配置约束：Memory 内部所有模型调用必须使用独立 Memory 配置，不再复用主 `chat` 模型，也不挂在主 `models.json` routing 上。

## 当前完成度判断

当前 Memory 已经具备可运行主链路：

- 独立 `tiangong-memory` crate、Actor/Handle、SQLite、Tantivy、Injection 已落地。
- Core 已按 `workspace_id` 缓存 `MemoryHandle`，长生命周期 GUI/Server 进程不会把不同 workspace 误绑定到首个 Memory Actor。
- Micro 写入链路已经覆盖 `TurnResult -> EpisodeWriter -> SQLite/Tantivy/向量索引`。
- `recall_memory` 已作为 Tool 化按需回忆入口，结果作为消息上下文注入，不追加到稳定 system prompt。
- 结构化产物记忆已经覆盖媒体 URL、文件路径和工具结果摘要。
- Meso 第一阶段已能从近期 Episode 提炼 Entity/Decision，写入 SQLite/Tantivy，并更新 Workspace Injection。
- TCP loopback IPC、远端 `MemoryHandle`、leader election 和 follower failover 已有测试覆盖。

阶段完成度估算：

| 阶段 | 当前判断 | 说明 |
|------|----------|------|
| Phase A | 已完成 | 独立 crate、Injection、SQLite、Actor/Handle 主体均可用 |
| Phase B | 基本完成 | Episode 写入、Tantivy、IPC/election 已闭环，仍需主入口切换到选举模式 |
| Phase C | 约 65%-75% | 内置向量索引、可选 Qdrant、Deep Recall、Meso 均已有实现，但真实模型路径和质量评测不足 |
| Phase D | 刚起步 | Meta 仅有低活跃归档，RerankProvider、Workspace Index、过时检测尚未完整实现 |

## 专用 Memory LLM 约束

Memory 不能再把主对话模型当成内部推理模型使用。主 `chat` 模型的职责是完成用户对话、工具决策和最终回复；Memory LLM 的职责是低成本、可控地处理记忆内部任务。

### 配置要求

Memory runtime 模型配置必须独立保存在 `~/.tiangong/memory/config.json`，由 `tiangong-memory` 提供配置类型、加载、保存和 `MemoryOptions` 转换能力。主 `models.json` 不再包含 `memory` capability 或 `routing.memory`；前端配置入口放在 LLM 配置下，选择项来自已有 Models，保存时由 Tauri 解析为 Memory runtime 端点配置。

示例：

```json
{
  "model": {
    "base_url": "https://api.example.com/v1",
    "api_key": "${MEMORY_API_KEY}",
    "model": "memory-small",
    "protocol": "openai_compatible",
    "timeout_ms": 60000
  },
  "embedding": {
    "base_url": "https://api.example.com/v1",
    "api_key": "${MEMORY_API_KEY}",
    "model": "bge-m3",
    "protocol": "openai_compatible",
    "timeout_ms": 60000,
    "dimension": 1024
  },
  "vector_mode": "embedded"
}
```

Memory 内部文本生成任务只允许读取独立 Memory LLM 配置：

- EpisodeWriter 结构化提取。
- Recall anchor 规划。
- Deep Recall 裁决。
- Recall 结果整理。
- Meso Entity/Decision 提炼。

### 降级规则

- 已配置 Memory LLM：使用专用 Memory LLM。
- 未配置 Memory LLM：Memory LLM 步骤降级为规则策略，并记录日志提示缺少 Memory LLM 配置。
- 禁止行为：未配置 Memory LLM 时静默复用 `chat` 主模型或旧 `lite` 模型。

### 设计动机

- 避免 Memory 的内部整理消耗主模型预算。
- 避免主模型能力、成本和延迟变化影响 Memory 稳定性。
- 允许用户为 Memory 单独选择小模型、本地模型或低成本服务。
- 让 Memory 的质量问题可以独立观测、独立调参、独立回退。

## 当前缺口清单

### P0：入口运行时缺口

1. Core 当前按 workspace 缓存 `MemoryHandle`，但仍直接调用 `start_with_options()`。
2. `tiangong-memory::start_or_connect()`、TCP bridge、leader election 和 follower failover 已可用，但尚未成为 CLI/GUI/Server 主入口默认路径。
3. 多进程同时运行 GUI + CLI + Server 时，理论上仍可能各自启动本地 Actor，而不是统一连接同一个 workspace leader。

验收标准：

- 所有入口统一通过 `start_or_connect()` 获取 Memory。
- 同一 workspace 同时启动多个入口时只有一个 leader，其余入口使用 remote handle。
- leader 退出后 follower 自动接替，并且接替前后写入/召回都可用。

### P0：Memory 独立模型配置缺口（已完成）

已完成：

- `tiangong-memory` 已增加独立 `MemoryConfig`，持久化到 `~/.tiangong/memory/config.json`。
- Memory LLM、Embedding、Rerank 配置已从主 `models.json` / routing 拆出。
- `CoreConfig::to_memory_options()` 只读取 Memory 独立配置，不再从主模型配置派生。
- Tauri 已提供 `get_memory_config` / `set_memory_config`。
- 前端已将 Memory 配置放到 LLM 配置下，并从已有 Models 中选择 Memory LLM、Embedding、Rerank。
- EpisodeWriter、Recall anchor 规划、Deep Recall 裁决、Recall synthesis 和 Meso 提炼共享的 Memory model 来源已收口到专用 Memory LLM。
- 未配置 Memory LLM 时，MemoryOptions 不再携带文本模型，上述步骤全部走规则 fallback。
- 已补测试禁止未配置 Memory LLM 时静默复用 `chat` 主模型或旧 `lite` 模型。

### P1：真实模型路径验证不足

1. EpisodeWriter、Deep Recall、Recall synthesis、Meso LLM 提炼已有代码路径，但缺少默认测试链覆盖。
2. 当前集成测试主要验证规则 fallback 和结构链路，真实 LLM 的 JSON 稳定性、token 成本和失败回退还缺少样例集。
3. Deep Recall 的真实效果还没有通过跨会话、跨产物、跨 Entity/Decision 的固定场景评测。

验收标准：

- 增加可手动运行的 Memory LLM smoke test。
- 固定 5-10 个历史指代样例，验证输出只包含增量记忆。
- 记录 Memory LLM token 使用和延迟，确认不会拖慢主对话链路。

### P1：混合检索质量缺口

1. 内置 flat 向量索引已实现，但真实 embedding 测试被标记为 ignored。
2. Qdrant 后端具备 connect/upsert/search，但还缺少归档删除同步、服务配置说明和主链路验证。
3. 召回质量缺少评测集，当前只能证明链路可运行，不能证明排序质量稳定。

验收标准：

- 默认测试链覆盖无外部服务的 embedded vector mock 或 deterministic provider。
- ignored 的真实 embedding 测试保留为手动验证，并补充运行说明。
- 建立小型 recall benchmark，比较 BM25-only 与 hybrid 的命中率。

### P1：Meta 与生命周期缺口

1. Meta 当前只有低活跃节点归档。
2. 已归档节点从向量索引删除的能力不完整，外部 Qdrant delete 仍是占位。
3. 过时检测尚未覆盖文件删除、路径失效、项目归档、产物 URL 过期。

验收标准：

- 归档时 SQLite、Tantivy、embedded vector、Qdrant 状态一致。
- 文件路径和媒体产物能做基本可达性检查。
- Meta 执行结果进入可观测日志和指标。

### P2：Workspace Index 缺口

1. 工作区文件索引和符号索引仍停留在设计文档。
2. Recall 尚不能从文件树、符号表、模块关系中补充上下文。
3. 文件变更后的增量更新策略尚未落地。

验收标准：

- 最小文件树索引可生成、可查询、可按 workspace 隔离。
- Rust 符号索引至少覆盖 `mod/fn/struct/enum/trait`。
- Recall 可以在用户询问“之前改过哪个模块”时结合 Memory 与 workspace index 返回线索。

## 建议推进顺序

1. 先实现 Memory 独立模型配置，阻断继续复用主模型。
2. 将 Core Memory 启动入口切换为 `start_or_connect()`，让 IPC/election 真正进入主链路。
3. 补真实 Memory LLM smoke test 和固定回忆样例集。
4. 补 embedded vector 的可重复测试，再处理 Qdrant 删除和服务化配置。
5. 最后推进 Meta 完整生命周期与 Workspace Index。

## 文档同步要求

后续实现上述任一缺口时，需要同步更新：

- `docs/requirements.md`
- `docs/memory-system/README.md`
- `docs/memory-system/09-分阶段落地路径.md`
- `docs/memory-system/11-当前开发状态与接手指南.md`
- 本文档的缺口状态
