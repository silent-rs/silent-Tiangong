# RFC 0012: Workspace/Session 索引系统

## 元数据

| 字段 | 值 |
|------|------|
| 编号 | 0012 |
| 状态 | 草案 |
| 创建日期 | 2026-05-21 |
| 作者 | Hubert Shelley |
| 依赖 | RFC 0004（全栈平台重构） |

## 概述

为天工平台引入 Workspace 级别和 Session 级别的文件/对话内容索引系统。该索引系统嵌入 `tiangong-core` 层，在对话上下文不足以支撑推理时，按「索引 → Memory → 网络检索」的优先级链进行数据补充。

## 背景与动机

当前 recall 流程仅依赖 Memory（长期记忆）系统进行历史信息检索。但在以下场景中存在明显不足：

1. **Workspace 文件感知不足**：用户在工作区中创建/修改的文件内容不会自动进入记忆系统，Agent 无法快速感知项目结构变化
2. **Session 对话历史丢失**：会话中的关键决策、技术选型、问题排查过程等未沉淀为可检索内容，跨会话无法复用
3. **检索优先级不清晰**：当前所有检索都走 Memory，缺少从近到远的分级检索策略

### 设计原则

- **索引不存入 Memory**：索引是独立于记忆系统的数据层，职责不同——索引是结构化的当前状态快照，记忆是语义化的历史经验沉淀
- **嵌入 Core 层**：索引管理逻辑位于 `tiangong-core`，不依赖 `tiangong-memory` crate
- **共享实例**：同一 Workspace 的多个 Session 共享同一个 Workspace 索引实例
- **按需更新**：通过文件监听实时更新 Workspace 索引，Session 索引在对话结束时批量更新

## 架构设计

### 检索优先级链

```
用户提问
  → 1. 当前对话上下文（已有）
  → 2. Workspace 索引（文件树 + 代码符号 + 文件内容片段）
  → 3. Session 索引（历史会话关键内容）
  → 4. Memory（长期记忆召回）
  → 5. 网络检索（外部数据补充）
```

### 模块划分

```
tiangong-core/
  src/
    index/                        # 新增：索引管理模块
      mod.rs                      # 模块入口，IndexManager 定义
      workspace_index.rs          # Workspace 索引逻辑
      session_index.rs            # Session 索引逻辑
      tantivy_schema.rs           # Tantivy Schema 定义
      watcher.rs                  # 文件监听（notify crate）
```

### IndexManager

```rust
/// 索引管理器，维护 Workspace 和 Session 索引实例
///
/// 同一 Workspace 的多个 Session 共享同一个 WorkspaceIndex 实例。
/// IndexManager 以 Arc 引用计数管理生命周期。
pub struct IndexManager {
    /// workspace_id → WorkspaceIndex 的共享实例
    workspace_indices: DashMap<String, Arc<WorkspaceIndex>>,
    /// session_id → SessionIndex
    session_indices: DashMap<String, SessionIndex>,
    /// 索引存储根目录
    base_dir: PathBuf,
}

impl IndexManager {
    /// 获取或创建指定 Workspace 的索引实例
    pub fn get_or_create_workspace_index(
        &self,
        workspace_id: &str,
        root_path: &Path,
    ) -> Arc<WorkspaceIndex>;

    /// 获取或创建指定 Session 的索引实例
    pub fn get_or_create_session_index(
        &self,
        session_id: &str,
        workspace_id: &str,
    ) -> &SessionIndex;

    /// 移除已关闭的 Session 索引
    pub fn remove_session_index(&self, session_id: &str);

    /// 移除不再活跃的 Workspace 索引（引用计数为 1 时）
    pub fn try_remove_workspace_index(&self, workspace_id: &str);
}
```

## Workspace 索引

### 索引内容

| 类别 | 字段 | 说明 |
|------|------|------|
| 文件树 | path, file_type, size, modified_at | 目录结构快照 |
| 代码符号 | name, kind, file_path, line_range, signature | Rust 函数/结构体/枚举/trait |
| 文件内容片段 | path, content_hash, snippet, language | 文件关键内容片段（前 N 行或摘要） |

### 存储方式

- **Tantivy 索引**：存储路径 `~/.tiangong/index/workspaces/{workspace_id}/tantivy/`
- **元数据 JSON**：存储路径 `~/.tiangong/index/workspaces/{workspace_id}/meta.json`

### 更新触发

采用文件监听（`notify` crate）实时更新：

```
文件创建/修改/删除
  → notify 事件
  → 防抖（300ms）
  → 增量更新 Tantivy 文档
  → 更新元数据快照
```

### Tantivy Schema

```rust
fn workspace_schema() -> Schema {
    let mut schema_builder = Schema::builder();
    // 通用字段
    schema_builder.add_text_field("path", STRING | STORED);
    schema_builder.add_u64_field("modified_at", INDEXED | STORED);
    // 文件内容
    schema_builder.add_text_field("content", TEXT | STORED);
    schema_builder.add_text_field("language", STRING);
    // 代码符号
    schema_builder.add_text_field("symbol_name", TEXT);
    schema_builder.add_text_field("symbol_kind", STRING);
    schema_builder.add_u64_field("symbol_line_start", INDEXED);
    schema_builder.add_u64_field("symbol_line_end", INDEXED);
    schema_builder.add_text_field("symbol_signature", TEXT);
    schema_builder.build()
}
```

### Scope 查询

Workspace 索引支持 Scope 感知查询，通过 Tantivy 的 filter 实现：

```rust
/// Scope 类型
enum IndexScope {
    /// 仅当前 Workspace
    Workspace(String),
    /// 所有 Workspace
    Global,
}

/// Scope 感知的 Tantivy 查询
fn build_scope_query(
    searcher: &Searcher,
    scope: &IndexScope,
    query: &str,
) -> Box<dyn Query> {
    let text_query = TermQuery::from(query);
    let scope_filter = match scope {
        IndexScope::Workspace(id) => {
            // 通过 meta.json 中的 workspace_id 关联
            // Tantivy 索引按 workspace_id 隔离目录，无需额外过滤
            None
        }
        IndexScope::Global => None,
    };
    text_query
}
```

### 索引限制

| 参数 | 值 | 说明 |
|------|------|------|
| max_entries | 5000 | 单个 Workspace 最大索引条目数 |
| max_depth | 8 | 目录遍历最大深度 |
| max_file_size | 2MB | 单文件最大索引大小 |
| snippet_lines | 50 | 文件内容片段最大行数 |
| debounce_ms | 300 | 文件变更防抖间隔 |

## Session 索引

### 索引内容

| 类别 | 字段 | 说明 |
|------|------|------|
| 对话摘要 | session_id, summary, topics, created_at | 会话级别的摘要信息 |
| 关键消息 | role, content, timestamp, importance | 高重要性消息（决策、结论、错误修复） |
| 提及实体 | entity_name, entity_type | 会话中提及的文件、函数、概念等 |

### 更新时机

- **实时更新**：每个 Turn 完成后，提取关键信息写入索引
- **批量更新**：会话结束时，生成完整的会话摘要并更新

### 存储方式

- **Tantivy 索引**：存储路径 `~/.tiangong/index/sessions/{session_id}/tantivy/`
- **元数据 JSON**：存储路径 `~/.tiangong/index/sessions/{session_id}/meta.json`

### Tantivy Schema

```rust
fn session_schema() -> Schema {
    let mut schema_builder = Schema::builder();
    schema_builder.add_text_field("session_id", STRING | STORED);
    schema_builder.add_text_field("workspace_id", STRING | STORED);
    schema_builder.add_text_field("content", TEXT | STORED);
    schema_builder.add_text_field("role", STRING);
    schema_builder.add_u64_field("timestamp", INDEXED | STORED);
    schema_builder.add_text_field("topics", TEXT);
    schema_builder.add_f64_field("importance", INDEXED | STORED);
    schema_builder.build()
}
```

## 检索集成

### 在 RuntimeEngine 中的集成点

```
RuntimeEngine::execute_turn_with_streaming
  → 构建上下文时：
    1. 检查当前对话上下文是否充分
    2. 如不充分，查询 Workspace 索引
    3. 如仍不充分，查询 Session 索引
    4. 如仍不充分，执行 Memory recall
    5. 如仍不充分，执行网络检索
```

### 查询接口

```rust
/// 索引查询请求
pub struct IndexQuery {
    pub query: String,
    pub scope: IndexScope,
    pub limit: usize,
    pub min_score: f64,
}

/// 索引查询结果
pub struct IndexHit {
    pub source: IndexSource,
    pub path: Option<String>,
    pub content: String,
    pub score: f64,
    pub snippet: Option<String>,
}

/// 索引来源
pub enum IndexSource {
    Workspace,
    Session,
}

impl IndexManager {
    /// 按优先级链查询：先 Workspace，再 Session
    pub async fn search(&self, query: &IndexQuery) -> Vec<IndexHit> {
        let mut hits = Vec::new();

        // 1. Workspace 索引
        if let Some(ws_index) = self.get_workspace_index(&query.scope) {
            hits.extend(ws_index.search(query).await);
        }

        // 2. Session 索引
        if hits.len() < query.limit {
            hits.extend(self.search_sessions(query, query.limit - hits.len()).await);
        }

        hits
    }
}
```

## 生命周期管理

### Workspace 索引

```
首次访问 Workspace
  → IndexManager.get_or_create_workspace_index()
  → 检查磁盘缓存
    → 有缓存：加载 Tantivy 索引，启动文件监听
    → 无缓存：全量扫描，构建索引，启动文件监听

Workspace 不再活跃
  → 引用计数降为 0
  → 停止文件监听
  → 保留磁盘缓存（下次访问可快速恢复）

磁盘缓存清理
  → 超过 30 天未访问的 Workspace 索引自动清理
```

### Session 索引

```
Session 创建
  → IndexManager.get_or_create_session_index()
  → 创建空索引

Turn 完成
  → 提取关键信息
  → 增量写入索引

Session 结束
  → 生成完整摘要
  → 写入索引
  → 保留磁盘缓存

Session 删除
  → 删除索引目录
```

## 与 Memory 系统的边界

| 维度 | 索引系统 | Memory 系统 |
|------|----------|-------------|
| 数据性质 | 结构化的当前状态 | 语义化的历史经验 |
| 更新方式 | 自动（文件监听/Turn 触发） | 混合（自动 rumination + 手动） |
| 查询方式 | Tantivy 全文检索 | Tantivy + LanceDB 双引擎 |
| 生命周期 | 与 Workspace/Session 绑定 | 持久化（可归档） |
| 所属 crate | tiangong-core | tiangong-memory |
| 检索优先级 | 高（步骤 2-3） | 中（步骤 4） |

## 依赖

| 依赖 | 用途 | 是否新增 |
|------|------|----------|
| `tantivy` | 全文索引引擎 | 已有（Memory 使用） |
| `notify` | 文件系统监听 | 新增 |
| `dashmap` | 并发 HashMap | 已有 |

## 存储结构

```
~/.tiangong/
  index/                         # 新增：索引根目录
    workspaces/
      {workspace_id}/
        tantivy/                 # Tantivy 索引数据
        meta.json                # 元数据（条目数、更新时间等）
    sessions/
      {session_id}/
        tantivy/                 # Tantivy 索引数据
        meta.json                # 元数据
```

## 开放问题

1. **文件内容索引深度**：是否需要对所有文本文件进行内容索引，还是仅索引文件头/摘要？全量索引可能消耗大量存储
2. **Session 索引清理策略**：归档的 Session 是否保留索引？保留多久？
3. **跨 Workspace 查询**：当用户在多个 Workspace 间切换时，是否需要跨 Workspace 联合查询？
4. **索引一致性**：Workspace 索引和 Memory 中的同一文件信息可能不一致，如何处理？

## 实施计划

### Phase 1：Workspace 文件树索引

- 实现 `IndexManager` 基础框架
- 实现 Workspace 文件树扫描和 Tantivy 索引构建
- 实现文件监听（notify）+ 防抖更新
- 集成到检索优先级链

### Phase 2：Workspace 代码符号索引

- 添加 Rust 代码符号解析（tree-sitter）
- 符号信息写入 Tantivy 索引
- 支持按符号名/签名查询

### Phase 3：Session 对话索引

- 实现 Session 索引构建
- Turn 结束后提取关键信息写入
- 支持跨 Session 查询

### Phase 4：优化与整合

- 磁盘缓存管理与清理
- 查询性能优化
- 与 Memory 系统的联合查询策略
