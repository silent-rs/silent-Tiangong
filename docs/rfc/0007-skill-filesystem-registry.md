# RFC-0007：基于文件系统的 Skill 实时加载机制

- 状态：Accepted
- 发起：2026-03-27
- 修订 RFC：[RFC-0003 Skill 市场与 MCP 依赖](./0003-skill-market.md)
- 关联 RFC：[RFC-0005 事件循环运行时](./0005-event-loop-runtime.md) / [RFC-0006 Core Config Provider](./0006-core-config-provider.md)

---

## 1. 背景

RFC-0003 为 Skill 设计了"注册表 + 锁文件"的持久化模型：

- `~/.tiangong/skills.json`：内存态 `SkillsConfig.installed[]` 的序列化落盘。
- `~/.tiangong/skills/skills-lock.json`：安装产物的锁定快照。
- `~/.tiangong/skills/installed/<id>/<version>/`：按版本隔离的安装目录。

实践中暴露出若干问题：

1. **双写一致性难维护**。内存状态、`skills.json`、`skills-lock.json` 三者需同步写入，任一持久化链路出错都会导致注册视图错乱；外部手动修改任一文件会被下一次 `persist_app_only()` 覆盖。
2. **"注册入口"与"文件落地"解耦过度**。文件系统中已经存在完整 Skill 目录时，仍无法被识别——必须再次走 `install_skill` 才能登记，用户心智负担高。
3. **版本目录造成冗余**。本地用户场景下通常只保留单一版本，`<id>/<version>/` 的两级目录反而让手动拖拽、Git 克隆等"裸部署"方式变得繁琐。
4. **实时性不足**。修改 Skill 内容（如 `SKILL.md`、脚本文件）后需要重启应用或重跑 `install_skill` 才会被加载。
5. **激活成本被前置**。即便用户一次会话只用到一个 Skill，应用启动时仍需加载全部已注册 Skill 的元数据到内存。

本 RFC 重新设计 Skill 的登记与加载机制，用**文件系统即注册表 + 按需实时加载**替代锁文件注册。

---

## 2. 目标与非目标

### 2.1 目标

1. **以 `~/.tiangong/skills/<id>/` 为唯一事实源**，移除 `skills-lock.json` 与 `skills.json.installed[]` 对注册状态的持有。
2. 支持用户通过"直接移动目录"完成安装：将 Skill 目录拷贝/移动到 `skills/` 下即可生效，无需命令。
3. Skill 元数据在**被激活时才加载**（命令调用、`@skill` 提及、检索匹配），降低启动开销。
4. 取消按版本的二级目录，Skill 目录名即 `<skill_id>`，版本信息保留在 `skill.toml` 中仅作展示用途。
5. Skill 的启停状态（`available`）直接内嵌在 `skill.toml` 中，不再依赖外部状态文件。
6. 保持 Skill → MCP 的映射与 `mcp-lock.json` 机制不变（MCP 仍需锁文件做引用计数与事务）。

### 2.2 非目标

- 不涉及 MCP 的锁机制变更（`mcp-lock.json` 继续存在）。
- 不重新设计 Skill 清单格式（`skill.toml` / `SKILL.md` 保持 RFC-0003 约定，仅新增 `available` 字段）。
- 不引入远程 registry（仍由 RFC-0003 Phase C 覆盖）。
- 不处理多版本并存场景（本地单机不需要）。

---

## 3. 核心变更一览

| 维度 | 旧机制（RFC-0003） | 新机制（RFC-0007） |
|------|------|------|
| 注册事实源 | `skills.json` + `skills-lock.json` | `~/.tiangong/skills/<id>/` 目录存在性 |
| 安装目录 | `installed/<id>/<version>/` | `skills/<id>/` |
| 安装入口 | 必须 `install_skill` | ① `install_skill` 复制目录；② 用户手动移动目录 |
| 启停状态 | 写入 `skills.json.installed[].enabled` | 写入 `skills/<id>/skill.toml` 的 `available` 字段 |
| 元数据加载 | 启动时全量加载到内存 | 激活时实时读取 `skill.toml` / `SKILL.md` |
| 卸载 | 命令 + 锁文件清理 | 删除 `skills/<id>/` 目录即可 |
| MCP 依赖锁 | `mcp-lock.json`（保留） | `mcp-lock.json`（保留，不变） |

---

## 4. 目录结构

### 4.1 用户目录布局

```text
~/.tiangong/
  app.json                    # 应用状态（不再含 skills.installed[]）
  mcp.json                    # 用户手动 MCP server 配置
  skills/
    mcp-lock.json             # MCP 依赖锁（保留）
    web-search/               # Skill ID 即目录名，直接平铺
      skill.toml              # 含 available 字段
      SKILL.md
      web_search.py
      .env.local              # 运行期注入的环境变量（保留）
    volcengine-video/
      skill.toml
      SKILL.md
      ...
    dingtalk-api-1/
      skill.toml
      SKILL.md
      ...
```

### 4.2 `skill.toml` 新增字段

```toml
id = "web-search"
name = "联网搜索"
version = "1.0.0"
description = "通过搜索引擎联网搜索信息，获取实时网页内容。"
entry = "SKILL.md"
available = true              # 新增：是否可被激活，默认 true

[source]
type = "local"
value = "/abs/path/to/source"

[permissions]
fs_read = []
fs_write = []
cmd_exec = []
net = []
```

**`available` 字段语义：**

| 值 | 含义 |
|----|------|
| `true`（默认） | Skill 可被 `@` 提及、命令调用、检索匹配正常激活 |
| `false` | Skill 已安装但被禁用，不参与检索匹配，不会被 `@` 激活，不出现在 Agent 可用工具列表中 |

- 该字段是 Skill 启停状态的**唯一来源**，不再依赖外部文件。
- `set_skill_enabled(id, enabled)` 仅修改 `skill.toml` 中的 `available` 字段，不触发全局持久化。
- 缺失时按 `true` 处理。

### 4.3 目录名规则

- 目录名必须等于 `skill.toml` 中的 `id`，否则扫描时视为非法目录并跳过（带审计告警）。
- `id` 是稳定机器标识（slug），用于目录名、`@skill` 引用、MCP 托管名前缀和审计 key；`name` 仅用于 UI 展示，可本地化或调整，不参与寻址。
- 不接受 `<id>/<version>/` 的旧布局；迁移器会在首次启动时处理（见 §9）。
- 系统始终只使用 `skills/<id>/` 当前内容，`skill.toml.version` 仅用于展示、审计和诊断，不参与版本选择或运行期回滚。

---

## 5. 扫描与加载机制

### 5.1 注册视图构建

定义 `SkillRegistryView`：

```rust
pub struct SkillEntry {
    pub id: String,
    pub dir: PathBuf,
    pub manifest_mtime: SystemTime,   // skill.toml 的 mtime
}

pub struct SkillRegistryView {
    entries: HashMap<String, SkillEntry>,  // 仅索引，不含完整元数据
    scanned_at: Instant,
}
```

- **轻量扫描**：只读取目录名 + `skill.toml` 的 `mtime`，不解析内容。
- **缓存时效**：`scanned_at` 早于阈值（默认 2s）则重用；否则重新扫描。
- **触发点**：
  1. 应用启动时一次。
  2. 每次 `@skill` 提及或命令调用前检查。
  3. UI 打开 Skill 面板时主动刷新。
  4. 显式调用 `refresh_skills()` 命令。

### 5.2 按需实时加载

```rust
pub struct LoadedSkill {
    pub manifest: SkillManifest,       // skill.toml 解析结果
    pub readme: String,                // SKILL.md 全文
    pub loaded_at: Instant,
    pub source_mtime: SystemTime,      // 用于判断是否需要重新加载
}
```

- `SkillService::get(id)` 的行为：
  1. 从 `SkillRegistryView` 取目录信息。
  2. 若内存已有 `LoadedSkill` 且 `manifest_mtime` 未变化 → 命中缓存。
  3. 否则重新读取 `skill.toml` + `SKILL.md` 并更新缓存。
- LRU 容量默认 32，超出后按 `loaded_at` 淘汰。
- 禁止把未激活 Skill 的 `SKILL.md` 全文纳入系统提示词，避免启动期膨胀。

### 5.3 激活时机

| 激活方式 | 触发点 | 加载粒度 |
|----------|--------|----------|
| `@skill-id` 提及 | 用户消息解析器识别到 `@<id>` 时 | 该 id 的完整 `LoadedSkill` |
| 命令调用 | Agent 计划调用某个 Skill 脚本时 | 该 id 的完整 `LoadedSkill` |
| 检索匹配 | `SkillsConfig.max_matches` 命中关键字 | 命中条目的完整 `LoadedSkill` |
| 管理面板 | UI 渲染列表 | 所有 Skill 的 `SkillEntry` + 懒加载详情 |

### 5.4 并发与一致性

- `SkillRegistryView` 读多写少，采用 `Arc<RwLock<...>>`。
- `LoadedSkill` 缓存采用 `DashMap<String, Arc<LoadedSkill>>`。
- 文件变更检测以 `mtime` 为准，不引入 `inotify/FSEvents`（首期）；Phase B 再评估。

---

## 6. 生命周期操作

### 6.1 安装

保留两条路径：

**路径 A — 命令式安装（`install_skill`）**

1. 解析源（目录/zip）并做校验。
2. 在目标目录 `skills/<id>/` 已存在时：
   - 保留 `.env.local`。
   - 保留 `available` 字段原值（用户手动禁用的不被覆盖）。
   - 删除旧目录其它内容（等价于"原地升级"）。
3. 复制源内容到 `skills/<id>/`。
4. 处理 MCP 依赖（见 §7）。
5. 触发一次 `SkillRegistryView` 重扫。

**路径 B — 手动安装（目录拷贝）**

- 用户将完整 Skill 目录放到 `skills/<id>/` 下。
- `skill.toml` 中无 `available` 字段时，扫描器按默认值 `true` 处理。
- **注意**：手动安装不会自动处理 `skill.toml` 中声明的 MCP 依赖；激活时若检测到缺失的受管 MCP，弹出提示让用户显式补装。

### 6.2 启停

- `set_skill_enabled(id, enabled)` 仅修改 `skills/<id>/skill.toml` 中的 `available` 字段。
- 不再触发 `persist_app_only()` 对 `skills.json` 的整体改写。
- 禁用状态下 Skill 不参与检索匹配，也不会被 `@` 激活。

### 6.3 卸载

- `uninstall_skill(id)`：
  1. 递减 `mcp-lock.json` 中对应 MCP 引用计数，归零则真正移除。
  2. 删除 `skills/<id>/` 目录。
  3. 从内存缓存中驱逐。

- **用户直接 `rm -rf skills/<id>/`**：
  - 扫描器识别为已卸载。
  - 但 `mcp-lock.json` 中的引用计数不会自动递减 → 产生"孤儿受管 MCP"。
  - 解决：提供 `tiangong skill gc` 命令和启动期轻量检测（见 §8）。

### 6.4 升级

- 单版本模式下"升级"等同于"原地替换"，复用路径 A 的流程。
- `skill.toml.version` 仅作展示与审计用途，不参与目录寻址。

---

## 7. Skill → MCP 依赖处理

MCP 部分**基本保留 RFC-0003 §7 的设计**，仅做以下调整：

1. `managed_mcp_servers` 的存储位置从 `skills.json.installed[].managed_mcp_servers` 迁移到 `skills/<id>/skill.toml` 的 `[mcp]` 段。
2. `mcp-lock.json` 不变，仍记录安装路径与引用计数。
3. 手动安装（路径 B）不会自动装 MCP；激活时若 `requires.mcp` 有项不在 `mcp-lock.json` 中 → 返回 `SkillActivationError::MissingMcp { skill_id, missing: [...] }`，UI/CLI 弹确认后补装。
4. `mcp.json` 中由 Skill 托管的 server 仍以 `skill::<id>::<mcp_id>` 命名；扫描期发现"托管 server 对应的 Skill 目录已消失"时，加入 §8 的 GC 候选。

---

## 8. 一致性与 GC

由于用户可绕过命令手动增删目录，需要一个**弱一致性修复器**：

### 8.1 启动期检查（轻量）

1. 扫描 `skills/` 目录。
2. 对比 `mcp.json` 中所有 `skill::*::*` 命名的 server：
   - Skill 目录不存在 → 标记为孤儿，触发告警但不自动删除。
3. 对比 `mcp-lock.json`：
   - 所有 `ref_count > 0` 但无任何 Skill 声明引用的条目 → 标记为孤儿。

### 8.2 `tiangong skill gc`

- 打印孤儿受管 MCP 与孤儿锁条目。
- 用户确认后：
  - 删除孤儿 `skill::*::*` server。
  - 递减 `mcp-lock.json` 引用计数、归零则清理产物。

### 8.3 非阻塞原则

- 孤儿检测只产生告警 + 审计事件，不阻塞启动或 Skill 激活。
- 保障用户在任意时刻手动移动目录都不会把应用打挂。

---

## 9. 迁移方案

首次运行新版本应用时：

### 9.1 检测旧布局

- `skills/installed/<id>/<version>/skill.toml` 形态：存在子目录且该子目录包含 `skill.toml`。
- `~/.tiangong/skills.json` 存在且含 `installed[]`。
- `~/.tiangong/skills/skills-lock.json` 仅作为旧布局伴随文件处理，不允许单独触发迁移。

### 9.2 自动迁移步骤

1. 扫描所有 `skills/installed/<id>/<version>/`，按 `id` 聚合；若有多个版本 → 只选择一个版本迁移为当前安装版本，其余仅作为旧布局备份，不参与后续运行。
2. 将选中版本的内容**平铺**到 `skills/<id>/`（删除中间 `installed/` 和 `<version>/` 两层）。
3. 基于 `skills.json.installed[]` 中的 `enabled` 字段，写入 `skill.toml` 的 `available` 字段。
4. 将 `skills.json` 备份为 `skills.json.legacy`，从 `app.json` 中移除 `agent_config.skills.installed[]`（保留 `enabled`、`dirs`、`max_matches` 等配置字段）。
5. 删除 `skills-lock.json`（备份为 `skills-lock.json.legacy`），新机制不再读取或写入该文件。
6. 迁移过程生成 `skill_migration` 审计事件，记录每条 Skill 的新旧路径与结果。

### 9.3 失败回退

- 迁移任一步骤失败 → 不删除旧文件，仅写入 `migration-failed.lock`，UI 提示用户手动处理。
- 成功后保留 `.legacy` 备份 30 天，过期由 `tiangong skill gc` 清理。

---

## 10. 对 RFC-0003 的修订

本 RFC 生效后，RFC-0003 相关章节按以下方式调整：

- **§8 存储与锁文件**：删除 `skills-lock.json`，保留 `mcp-lock.json`；`installed/<id>/<version>/` → `skills/<id>/`。
- **§10 事务与回滚**：Skill 侧事务边界收敛到"目录复制 + `skill.toml` 字段更新"两步；MCP 侧事务不变。
- **§13 验收标准第 3 条**：由"三文件一致"调整为"`mcp.json` 与 `mcp-lock.json` 一致，且 `skills/` 目录扫描结果与内存缓存一致"。
- 其余章节（权限模型、审计事件、分阶段实施）保持有效。

---

## 11. Tauri Command 契约变更

| 旧命令 | 新命令 / 行为 |
|--------|----------------|
| `install_skill(path, enabled)` | 保留；内部改为复制到 `skills/<id>/` 并更新 `skill.toml` |
| `remove_skill(id)` | 保留；内部改为删除 `skills/<id>/` 并处理 MCP 引用计数 |
| `set_skill_enabled(id, enabled)` | 保留；只改写 `skills/<id>/skill.toml` 的 `available` 字段 |
| `list_skills()` | 改为返回扫描结果（`SkillEntry` 列表，不含 `SKILL.md` 全文） |
| `get_skill_detail(id)` | 改为触发 `LoadedSkill` 实时加载并返回完整信息 |
| **新增** `refresh_skills()` | 强制重扫 `skills/` |
| **新增** `gc_skills()` | 执行孤儿 MCP 与锁条目清理 |

前端调用层尽量保持兼容；仅 `list_skills` 的返回结构瘦身（不含 readme 全文），需要前端按 `get_skill_detail` 懒加载。

---

## 12. 安全与风险

### 12.1 仍然保留的约束（来自 RFC-0003）

- 所有路径 canonicalize。
- 权限摘要仅在激活 / 安装时展示并记录审计。
- MCP 命名冲突检测。

### 12.2 新增风险与对策

| 风险 | 对策 |
|------|------|
| 用户手动丢入恶意目录 | 首次激活时强制展示权限摘要并二次确认；审计事件标记为 `source=manual`。 |
| 手动删除导致孤儿 MCP | §8 GC + 启动期轻量检测。 |
| `mtime` 不可靠（某些网络盘） | 提供 `refresh_skills(force=true)` 绕过 mtime 判定。 |
| `skill.toml` 被误删 | 扫描器跳过该目录并记录审计告警。 |
| 目录名与 `skill.toml.id` 不一致 | 扫描器跳过并记录审计；CLI 提供 `tiangong skill doctor` 诊断。 |

---

## 13. 分阶段实施

### Phase A — 设计冻结与 scaffolding

- 本 RFC 合入 Draft → Accepted。
- `SkillRegistryView`、`LoadedSkill` 数据结构落地。
- `skill.toml` 新增 `available` 字段的解析与序列化。
- 单元测试覆盖扫描、缓存、mtime 失效。

### Phase B — 运行时接入

- `SkillService` 改造：所有 `install/remove/enable/list/get` 走新路径。
- 移除 `persist_app_only()` 对 `skills.installed[]` 的写入分支。
- 实现 §9 迁移器；新老数据并存时优先用新布局。

### Phase C — 外围能力

- `refresh_skills` / `gc_skills` Tauri 命令与 CLI 子命令。
- UI 管理面板接入懒加载。
- `skill doctor` 诊断工具。

### Phase D — 清理

- `skills.json.installed[]`、`skills-lock.json` 相关代码删除。
- `.legacy` 备份到期清理。
- 文档与示例 Skill 更新到新布局。

---

## 14. 验收标准

满足以下条件视为 RFC-0007 完成：

1. `skills/<id>/` 目录的存在性唯一决定 Skill 是否可被激活。
2. 用户手动拷贝/删除 Skill 目录后，下一次扫描或激活能立即感知，无需重启应用。
3. `skill.toml` 的 `available` 字段能完整表达 Skill 的启停状态。
4. MCP 依赖锁 (`mcp-lock.json`) 的引用计数与所有 Skill 目录中声明的 MCP 依赖汇总一致。
5. 从 RFC-0003 旧布局迁移到新布局的过程全自动，失败可回退并保留旧文件。
6. `SKILL.md` 仅在激活时才会被读入内存，冷启动不受已安装 Skill 数量线性影响。
7. 删除 `skills/<id>/` 后，`tiangong skill gc` 能清理对应的孤儿 MCP 托管条目。

---

## 15. 未决问题

- **文件系统通知**：是否在 Phase C 引入 `notify` crate 做实时监听？对 macOS / Windows / Linux 差异如何处理？
- **多实例并发**：两个天工进程同时操作同一 `~/.tiangong` 目录的协调策略（文件锁？）。
- **Skill 目录只读快照**：是否需要在激活时对 `skill.toml` 做一次只读指纹以防止运行期被篡改？
- **Skill 子目录嵌套**：是否允许 `skills/<group>/<id>/` 的两级命名（目前只允许一级扁平 id）。

以上均不阻塞本 RFC 进入 Accepted，留待实施中再评估。
