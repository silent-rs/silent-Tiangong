# 天工智能体 Memory 系统设计（Progressive Memory）

## 一、设计目标

天工智能体的 Memory 系统目标不是“存更多”，而是：

- 低成本粗召回
- 按意图逐步深入
- 多层细节展开
- 上下文预算可控
- 可审计、可维护、可迁移

核心理念：

> Memory 不应一次性注入，而应像人类回忆一样逐步展开。

---

## 二、核心思想

从传统：

用户问题 → 检索 → 全量注入 Prompt

升级为：

用户问题
  → 锚点提取
  → 粗召回
  → 回忆规划
  → 定向展开
  → 必要时证据回放

---

## 三、记忆分层设计

### 1. Persona / Profile Memory

- 用户偏好
- 模型/厂商选择
- 输出风格

特点：稳定、小、常驻

---

### 2. Workspace Memory

- 项目结构
- 架构信息
- 部署环境

特点：与工作区强绑定

---

### 3. Episodic Memory

- 任务目标
- 执行步骤
- 成功/失败原因
- 产物

特点：自动沉淀，最重要

---

### 4. Entity Memory

实体类型：

- project / repo / server / skill / provider / document

特点：摘要注入 + 正文按需加载

---

### 5. Decision Memory

- 方案对比
- 最终选择
- 原因

特点：回答“为什么”

---

### 6. Evidence Memory

- 日志
- diff
- tool 调用
- 原始对话

特点：仅在需要时回放

---

### 7. Injection Memory（注入记忆层）

用于为不同 Agent / 模型提供“对话压缩后”或“新对话启动时”的必要上下文注入。

建议拆成三层：

- Profile 级注入：用户长期偏好、稳定规则、固定表达风格
- Workspace 级注入：项目背景、架构约束、环境约定、当前阶段目标
- Session 级注入：本轮任务摘要、最近决策、待办事项、关键上下文

建议为不同运行主体维护独立注入文件：

- `agent.md`：面向通用 Agent 运行时
- `claude.md`：面向 Claude / Claude Code 风格上下文
- `tiangong.md`：面向天工自身运行时与技能编排

特点：
- 体积小
- 高密度
- 适合在对话压缩后继续注入
- 适合在新对话开始时作为启动上下文

---

### 8. Workspace Index（工作区文件索引层）

用于索引当前对话所在工作区的真实文件状态，为 Recall 与执行阶段提供“当前事实源”。

它不是 Memory，而是与 Memory 并行的实时知识源。

适合索引的内容包括：

- 文件树与目录结构
- 文件类型与修改时间
- 代码符号（如函数、结构体、模块）
- 配置文件与脚本入口
- 文档与说明文件
- 按需展开的文件片段

特点：
- 实时性强
- 可变性高
- 作为 Ground Truth 使用
- 与记忆系统并行参与 Planner
- 不直接替代 Episodic / Decision / Entity Memory

---

## 四、目录结构

```
.tiangong/
  memory/
    profile/
      profile.md
      agent.md
      claude.md
      tiangong.md
    workspaces/
      <workspace_id>/
        workspace.md
        agent.md
        claude.md
        tiangong.md
        entities/
        episodes/
        decisions/
        evidence/
        indexes/
          lexical/
          vector/
          graph/
    sessions/
      <session_id>/
        summary.md
        agent.md
        claude.md
        tiangong.md
  workspace-index/
    <workspace_id>/
      file-tree.json
      symbols.json
      chunks/
      embeddings/
      cache/
```

---

## 五、核心数据结构（Rust）

### MemoryNode

```rust
pub struct MemoryNode {
    pub id: MemoryId,
    pub kind: MemoryKind,
    pub scope: MemoryScope,
    pub title: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub tags: Vec<String>,
    pub importance: f32,
    pub confidence: f32,
    pub created_at: i64,
    pub updated_at: i64,
    pub links: Vec<MemoryLink>,
}
```

---

### MemoryKind

```rust
pub enum MemoryKind {
    Profile,
    Workspace,
    Episode,
    Entity,
    Decision,
    Evidence,
}
```

---

### 分级内容

```rust
pub struct MemoryDepthContent {
    pub depth1: String,
    pub depth2: Option<StructuredView>,
    pub depth3: Option<EvidenceView>,
}
```

---

## 六、回忆流程

### 1. 锚点提取

提取：
- workspace
- entity
- intent
- keywords

---

### 2. 粗召回

输出候选摘要（Top-K）

---

### 3. 回忆规划

决定：
- 展开哪些记忆
- 展开深度

---

### 4. 定向展开

仅展开结构化内容，不直接注入全文

---

### 5. 证据回放

必要时加载：
- 日志
- diff
- 原始记录

---

### 6. 文件索引参与规则

当用户请求涉及以下场景时，应启用 Workspace Index：

- 当前代码或配置的真实状态查询
- 需要修改文件或生成 patch
- 需要定位函数、模块、配置项
- 需要验证当前实现是否符合历史设计
- 需要将历史决策与当前代码进行对照

当用户问题仅涉及历史经验、原因追溯、长期偏好时，可仅使用 Memory Recall，而不启用文件索引。

---

用户问题
  → 锚点提取
  → 粗召回（Memory）
  → 文件定位（Workspace Index）
  → 回忆规划（Planner）
  → 定向展开（记忆 + 文件）
  → 必要时证据回放

---

## 七、Recall Intent

```rust
pub enum RecallIntent {
    ContinueTask,
    WhyDecision,
    ErrorRecovery,
    FindObject,
}
```

---

## 八、预算控制

建议：

- 总预算：2000 tokens
- 摘要：300
- 结构化：900
- 证据：800

裁剪策略：

1. 删除低相关
2. 删除低 importance
3. 限制 depth

---

## 九、分层注入文件设计（agent.md / claude.md / tiangong.md）

除了 Progressive Recall 外，天工还需要一套“稳定、小体积、高密度”的注入上下文机制，用于：

- 对话压缩后继续保留必要背景
- 新对话启动时快速恢复关键上下文
- 针对不同 Agent / 模型使用不同注入格式

### 1. 为什么需要独立注入层

Memory 负责“可探索的长期记忆”，但很多信息并不适合每次都走完整 Recall 流程，例如：

- 用户长期偏好
- 项目固定约束
- 近期阶段目标
- 当前工作区规则
- 本轮任务关键结论

这些内容更适合作为“压缩后的稳定注入块”，在以下场景使用：

- 长对话被压缩后
- 新会话启动时
- 子 Agent 派生时
- 技能执行前快速注入时

### 2. 注入层级设计

建议分为三层：

#### Profile 级

面向用户长期不变或低频变化的信息：

- 语言偏好
- 输出偏好
- 常用技术栈
- 常用模型/平台
- 稳定安全规则

建议文件：

- `.tiangong/memory/profile/agent.md`
- `.tiangong/memory/profile/claude.md`
- `.tiangong/memory/profile/tiangong.md`

#### Workspace 级

面向当前项目 / 仓库 / 工作区的稳定背景：

- 项目目标
- 架构约束
- 目录约定
- 部署环境
- 当前阶段重点
- 常见风险

建议文件：

- `.tiangong/memory/workspaces/<workspace_id>/agent.md`
- `.tiangong/memory/workspaces/<workspace_id>/claude.md`
- `.tiangong/memory/workspaces/<workspace_id>/tiangong.md`

#### Session 级

面向当前任务或近期会话的高时效信息：

- 当前目标
- 最近决策
- 当前待办
- 已完成事项
- 暂存约束
- 下一步建议

建议文件：

- `.tiangong/memory/sessions/<session_id>/agent.md`
- `.tiangong/memory/sessions/<session_id>/claude.md`
- `.tiangong/memory/sessions/<session_id>/tiangong.md`

### 3. 三类注入文件的职责差异

#### agent.md

定位：通用 Agent 上下文注入

适合包含：
- 任务目标
- 工作区规则
- 执行边界
- 关键上下文摘要
- 可执行约束

特点：
- 中性
- 简洁
- 对运行时友好

#### claude.md

定位：Claude / Claude Code 风格注入

适合包含：
- 项目背景
- 编码约定
- 输出偏好
- 文件修改原则
- 沟通与解释风格

特点：
- 更贴近代码助手
- 更适合“继续这个仓库里的工作”
- 更强调约束和开发习惯

#### tiangong.md

定位：天工原生运行时注入

适合包含：
- 技能路由偏好
- 工作流阶段
- Memory 使用策略
- 当前推荐 Agent 行为
- Recall / Rumination 策略摘要

特点：
- 更贴近调度层
- 更适合技能编排和多 Agent 协作
- 更适合天工自己的控制面

### 4. 组合注入顺序

建议默认注入顺序：

1. Profile 级
2. Workspace 级
3. Session 级

最终形成：

```text
Profile Injection
  + Workspace Injection
  + Session Injection
```

当预算不足时，优先级建议为：

1. Session 级
2. Workspace 级
3. Profile 级

### 5. 生成与更新机制

这些注入文件不建议完全手写，应由系统自动生成并允许人工修订。

建议触发时机：

- 对话压缩后：更新 session 级注入文件
- 任务结束后：更新 session + workspace 级摘要
- 阶段里程碑后：更新 workspace 级注入文件
- 用户偏好变更后：更新 profile 级注入文件

### 6. 内容风格要求

注入文件必须遵循：

- 只保留高价值信息
- 尽量使用短句
- 避免冗余叙述
- 避免原始长日志
- 尽量结构化
- 可直接拼接进 Prompt

### 7. 示例

#### Profile / agent.md

```markdown
# Profile Injection
- 默认使用简体中文回复。
- 用户主要使用 Rust，偶尔使用 Python。
- 前端通常使用 Vue。
- 用户维护 Silent Rust Web 框架。
- 优先给出工程可落地方案。
```

#### Workspace / claude.md

```markdown
# Workspace Injection
- 当前项目：天工智能体。
- 当前重点：Progressive Memory 与注入层设计。
- 输出优先采用 Rust 风格模块边界。
- 文档优先面向可工程落地的架构设计。
- 修改时保持目录结构一致。
```

#### Session / tiangong.md

```markdown
# Session Injection
- 当前任务：设计多层级注入文件体系。
- 已决定采用 Profile / Workspace / Session 三层注入。
- 已决定为 agent.md / claude.md / tiangong.md 提供独立适配。
- 下一步建议：补充 Rumination 层与 Injection 层协同策略。
```

## 十、写入机制

### Episode 示例

```json
{
  "title": "Anthropic 兼容方案评估",
  "decision": "使用 async-anthropic",
  "reasons": ["快速交付", "维护成本低"]
}
```

---

## 十一、检索策略

- Lexical（关键词）
- Vector（语义）
- Graph（关系）
- Temporal（时间）
- Scope（工作区）

---

## 十二、Workspace Index（工作区文件索引）

### 1. 为什么需要独立文件索引层

Memory 解决的是“过去做过什么、为什么这么做”，但在执行型智能体场景下，还必须掌握“当前文件真实处于什么状态”。

因此，需要一个与 Memory 并行的 Workspace Index，用于提供当前工作区的 Ground Truth。

如果没有这一层，会出现：

- 记忆与当前代码状态不一致
- 无法精确定位需要修改的文件
- 无法验证历史设计是否已被正确实现
- 无法基于真实上下文生成可靠 patch

### 2. 职责边界

| 类型 | Memory | Workspace Index |
|------|--------|----------------|
| 关注对象 | 经验、决策、实体、偏好 | 当前文件、代码、配置、文档 |
| 是否长期 | 是 | 否 |
| 是否可变 | 低频 | 高频 |
| 是否权威 | 可能过时 | 当前事实源 |
| 是否参与 Recall | 是 | 是 |
| 是否参与 Rumination | 是 | 否 |

一句话：

> Memory 是经验，Workspace Index 是现实。

### 3. 三层索引建议

#### 1）结构索引（必须）

用于保存：
- 文件树
- 文件类型
- 路径
- 修改时间
- 大小

用途：
- 快速定位文件
- 帮助 Planner 决定是否需要读取文件

#### 2）轻量语义索引（推荐）

用于保存：
- 函数级或段落级 chunk
- embedding
- 关键词
- 符号信息

用途：
- 找到最相关文件或片段
- 不必一次性加载整个工作区

#### 3）按需读取层（关键）

在 Planner 确认后，按需读取：
- 某个文件
- 某个函数
- 某段配置
- 某个说明文档片段

用途：
- 为执行、分析、patch 生成提供真实上下文

### 4. 与 Recall / Planner 的协同

建议工作流：

1. 用户问题进入系统
2. Recall 先检索历史记忆
3. Planner 判断是否需要当前事实源
4. 若需要，则启用 Workspace Index
5. 将记忆与文件片段一起送入定向展开阶段

适合启用 Workspace Index 的问题：

- “帮我继续完善某个模块”
- “这个实现现在是否符合设计”
- “帮我改一下配置”
- “这个函数在哪个文件里”

不一定需要启用 Workspace Index 的问题：

- “上次为什么这么选型”
- “之前这个问题是怎么解决的”
- “我的长期偏好有哪些”

### 5. Rust 模块建议

```text
tiangong-workspace/
  src/
    index/
      file_tree.rs
      symbol_index.rs
      chunk_index.rs
    retrieve/
      search.rs
      rank.rs
    reader/
      file_reader.rs
      snippet.rs
```

### 6. Trait 建议

```rust
pub trait WorkspaceIndex {
    fn list_files(&self, workspace: &str) -> Vec<FileMeta>;
    fn search(&self, query: &str) -> Vec<FileCandidate>;
    fn get_symbols(&self, file: &str) -> Vec<Symbol>;
}
```

```rust
pub trait WorkspaceReader {
    fn read_file(&self, path: &str) -> String;
    fn read_snippet(&self, path: &str, range: Range) -> String;
}
```

### 7. 设计原则

- 文件索引不等于长期记忆
- 不做全量长文本注入
- 优先定位，再按需展开
- 让 Planner 决定是否启用文件索引
- 始终将当前文件视为 Ground Truth

---

## 十三、Rust 模块划分

```
tiangong-memory/
  recall/
  index/
  store/
  ingest/
  injection/
    builder/
    composer/
    updater/

tiangong-workspace/
  index/
  retrieve/
  reader/
```

---

## 十四、技能系统集成

每个 Skill 拥有独立记忆：

- 成功案例
- 失败案例
- 常见模式

此外，技能系统也应能消费分层注入文件：

- `agent.md`：作为通用任务执行注入
- `claude.md`：作为代码型子 Agent 注入
- `tiangong.md`：作为天工调度型技能注入

这样技能在新会话、压缩恢复、子任务派生时，都能快速获得必要上下文，而不需要完整重放历史对话。

同时，技能在执行涉及代码、配置、文档、脚本的任务时，也应能够通过 Workspace Index 获取当前工作区的真实文件状态，从而避免仅依赖历史记忆做出错误操作。

---

## 十五、Memory Rumination（反刍与反思层）

Progressive Recall 解决“如何回忆”，而 Rumination 解决“如何整理与修正记忆”。

该层用于让系统具备类似人类“复盘、反思、自我修正”的能力。

---

### 1. 目标

- 去除重复记忆
- 修正错误记忆
- 标记过时信息
- 提炼高层规律
- 检测冲突信息
- 控制记忆规模增长

---

### 2. 三层反刍结构

#### 1）Micro Reflection（任务级反思）

触发时机：单次任务结束后

输入：
- 当前对话
- 工具调用链
- 执行结果

输出：
- Episode
- Decision（如有）
- Entity 更新
- Evidence 引用

---

#### 2）Meso Rumination（阶段性整理）

触发时机：周期性（如每日/每周）

输入：
- 最近一段时间的 Episode
- 当前 Workspace 状态

输出：
- 去重后的记忆
- 合并摘要（Consolidated Memory）
- 冲突检测报告
- 归档建议

---

#### 3）Meta Reflection（长期反思）

触发时机：阶段里程碑 / 项目阶段结束

输入：
- 长期记忆集合
- 多阶段决策链
- 技能执行统计

输出：
- Best Practices
- Anti-patterns
- Skill 使用建议
- Workspace 规则沉淀

---

### 3. 核心能力

#### 去重（Deduplication）
合并语义重复或高度相似的记忆节点。

#### 纠错（Correction）
识别被新信息推翻的旧记忆，并标记为：
- outdated
- superseded

#### 抽象（Abstraction）
从多个 Episode 提炼规律，生成 Pattern Node。

#### 冲突检测（Conflict Detection）
识别同一实体或事实的矛盾记录，并生成 Conflict Node。

#### 压缩归档（Archival）
将低活跃但有价值的记忆移入归档区。

#### 可信度重估（Confidence Update）
根据新证据动态调整记忆置信度。

---

### 4. Memory 状态扩展

```rust
pub enum MemoryStatus {
    Active,
    Archived,
    Outdated,
    Superseded,
    Conflicted,
}
```

```rust
pub struct MemoryQuality {
    pub confidence: f32,
    pub freshness: f32,
    pub contradiction_score: f32,
}
```

---

### 5. Pattern 与 Conflict 节点

#### Pattern Node
用于表达经验规律，例如：
- 某技能在特定场景下容易失败
- 某类任务适合某模型

#### Conflict Node
用于记录冲突信息，例如：
- 同一实体属性不一致
- 决策状态冲突

---

### 6. 与 Injection 层的关系

Rumination 的输出将直接影响注入文件：

- 高价值稳定结论 → 写入 Profile / Workspace 注入
- 当前阶段结论 → 写入 Session 注入
- 错误或冲突信息 → 不进入注入层

---

### 7. 与 Workspace Index 的关系

Rumination 不直接处理 Workspace Index 中的实时文件索引，但可以基于文件变化事件生成新的 Episode、Decision、Entity 更新或 Pattern 总结。

也就是说：

- Workspace Index 提供“当前事实”
- Rumination 负责把“阶段性事实变化”沉淀成长期知识

---

### 8. 设计原则

- 不直接删除记忆，优先标记状态
- 区分事实、推断、规律
- 规律必须有多次证据支持
- 高影响记忆允许人工确认

---

### 9. 一句话总结

> Recall 让系统会想起过去，Rumination 让系统从过去中变得更聪明。

## 十六、总结

核心能力：

- Progressive Recall（渐进式回忆）
- Intent-aware（意图驱动）
- Evidence Replay（证据回放）
- Layered Injection（分层注入）
- Workspace Index（工作区事实索引）

一句话：

> 用最小上下文，逐步逼近最有价值的记忆，并通过分层注入与工作区事实索引，让新对话、压缩恢复与执行阶段都具备稳定起点。
