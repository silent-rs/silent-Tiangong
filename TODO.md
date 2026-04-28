# TODO - 天工当前开发任务

> 最后更新：2026-04-28
> 当前主线：工作空间与文件操作边界
> 参考：`PLAN.md`、`docs/requirements.md`

---

## 当前结论

接下来收口工作空间与文件操作边界。目标是把默认目录、读取能力和写入限制拆清楚：

- Desktop 模式从界面设置工作空间；CLI / Server 模式默认以进程当前运行目录作为工作空间。
- 用户未指定目录时，默认加载当前工作空间。
- 读取类操作不限制目录，便于必要时获取外部信息。
- 写入、修改、删除、补丁和有文件副作用的命令只允许当前工作空间、当前对话指定目录和 `~/.tiangong/skills`。
- `~/.tiangong/skills` 作为特殊可写目录，支持 Skill 故障时及时修复。

旧 RFC-0007 Skill 主线已迁出到 PR，本文档只保留当前工作空间主线真实开发差距。

---

## P0 - Phase A：工作空间边界收口

### 1. 同步需求边界

- [x] 在 `docs/requirements.md` 中补充工作空间与文件操作边界。
- [x] 明确 `~/.tiangong/skills` 是特殊可写范围。
- [x] 明确工作空间与当前对话目录分离，`session_cwd` 只表示当前对话目录。

### 2. 调整工具路径策略

- [x] Desktop 系统设置弹窗提供工作区目录设置入口。
- [x] Desktop 工作空间独立持久化到应用级状态，不复用 `session_cwd`。
- [x] 新对话默认复制当前工作空间作为对话目录。
- [x] 读取类工具允许访问工作空间外路径。
- [x] 写入类工具只允许当前工作空间和 `~/.tiangong/skills`。
- [x] 命令执行 cwd 必须限制在写入允许范围内。
- [x] shell 文件副作用命令的路径参数必须限制在写入允许范围内。
- [x] 保持用户未指定目录时默认使用当前工作空间。

### 3. 验证

- [x] `cargo check --workspace` 通过。

---

## 已完成：RFC-0007 Skill 文件系统注册表

### Phase A：设计冻结与基础结构

### 1. 接受 RFC-0007 并同步需求边界

- [x] 将 `docs/rfc/0007-skill-filesystem-registry.md` 状态从 Draft 调整为 Accepted。
- [x] 在 `docs/requirements.md` 中补充 Skill 文件系统注册表要求。
- [x] 在 `PLAN.md` 中把当前主目标切换到 RFC-0007。
- [x] 明确首期不引入文件系统通知，仅使用扫描 + mtime 缓存。
- [x] 明确 MCP 侧 `mcp-lock.json` 机制保持不变。

### 2. 定义 Skill 文件系统注册表数据结构

- [x] 新增或改造 `SkillEntry`，字段至少包含 `id`、`dir`、`manifest_mtime`。
- [x] 新增或改造 `SkillRegistryView`，只保存轻量索引，不保存 `SKILL.md` 全文。
- [x] 新增或改造 `LoadedSkill`，字段至少包含 `manifest`、`readme`、`loaded_at`、`source_mtime`。
- [x] 为注册表扫描结果定义非法目录、manifest 缺失、id 不一致等错误/告警类型。
- [x] 明确缓存阈值默认 2 秒，支持强制刷新绕过缓存。

### 3. 支持 `skill.toml.available`

- [x] 为 Skill manifest 增加 `available: bool` 字段。
- [x] `available` 缺失时按 `true` 处理。
- [x] manifest 序列化时保留或写入 `available`。
- [x] 注册表层提供 `set_available` / `write_skill_available` 写入能力。
- [x] `set_skill_enabled(id, enabled)` 只修改 `skills/<id>/skill.toml`。
- [x] `available=false` 时按需加载不读取 `SKILL.md` 正文。
- [x] 禁用 Skill 不参与 `@skill` 激活、检索匹配和 Agent 可用工具列表。

### 4. 实现轻量扫描与按需加载

- [x] 扫描 `~/.tiangong/skills/<id>/` 平铺目录。
- [x] 跳过 `mcp-lock.json`、隐藏文件、非目录和非法目录。
- [x] 校验目录名必须等于 `skill.toml.id`，不一致时跳过并记录告警。
- [x] `list_skills()` 返回轻量 `SkillEntry` / 摘要，不读取 `SKILL.md` 全文。
- [x] `get_skill_detail(id)` 触发 `skill.toml` + `SKILL.md` 实时加载。
- [x] 已加载 Skill 在 `manifest_mtime` 未变化时命中缓存。
- [x] 缓存容量默认 32，超出后按 `loaded_at` 做 LRU 淘汰。

### 5. Phase A 测试

- [x] 单元测试：扫描合法 `skills/<id>/skill.toml`。
- [x] 单元测试：跳过目录名与 manifest id 不一致的目录。
- [x] 单元测试：`available` 缺失默认 true。
- [x] 单元测试：`available=false` 不可激活。
- [x] 单元测试：mtime 变化后重新加载。
- [x] 单元测试：强制 refresh 绕过缓存。

---

## P1 - Phase B：运行时接入与迁移

### 6. 改造 SkillService 主链路

- [x] `install_skill(path, enabled)` 改为复制到 `skills/<id>/`。
- [x] 安装时目标目录已存在则保留 `.env.local`。
- [x] 安装时目标目录已存在则保留原 `available`，避免覆盖用户禁用状态。
- [x] 安装完成后触发 `SkillRegistryView` 重扫。
- [x] `remove_skill(id)` 改为删除 `skills/<id>/` 并驱逐缓存。
- [x] `list/get/enable/remove/install` 全部走文件系统注册表，不再依赖 `skills.json.installed[]`。

### 7. 移除 `skills.installed[]` 持久化写入

- [x] 删除或旁路 `persist_app_only()` 中对 `skills.installed[]` 的写入。
- [x] `app.json` / `skills.json` 只保留非注册状态配置，例如 `enabled`、`dirs`、`max_matches`。
- [x] 外部手动修改 `skills/` 目录后不会被下一次全局持久化覆盖。
- [x] 启动时优先使用新布局 `skills/<id>/`。

### 8. 实现旧布局迁移器

- [x] 检测 `skills/installed/<id>/<version>/skill.toml` 旧目录。
- [x] 检测旧 `skills.json.installed[]`。
- [x] 旧 `skills/skills-lock.json` 仅作为旧布局伴随文件处理，不单独触发迁移。
- [x] 多版本并存时优先选择 `skills.json` 登记版本。
- [x] 将选中版本平铺迁移到 `skills/<id>/`。
- [x] 将旧 `enabled` 写入新 `skill.toml.available`。
- [x] 将 `skills.json` 备份为 `skills.json.legacy`。
- [x] 将旧布局伴随的 `skills-lock.json` 备份为 `skills-lock.json.legacy` 后移除原文件。
- [x] 从 app 配置中移除 `agent_config.skills.installed[]`。
- [x] 迁移失败时写入 `migration-failed.lock`，不删除旧文件。
- [x] 生成 `skill_migration` 审计事件，记录新旧路径与结果。

### 9. Phase B 测试

- [x] 集成测试：命令式安装后目录落在 `skills/<id>/`。
- [x] 集成测试：手动拷贝目录后下一次扫描可见。
- [x] 集成测试：删除目录后下一次扫描不可见。
- [x] 集成测试：禁用状态通过 `skill.toml.available=false` 持久化。
- [x] 集成测试：旧 `<id>/<version>` 布局自动迁移。
- [x] 集成测试：迁移失败保留旧文件并写入失败锁。

---

## P2 - Phase C：外围命令、UI 与一致性修复

### 10. 刷新与 GC 命令

- [x] 新增 Tauri command：`refresh_skills()`。
- [x] 新增 Tauri command：`gc_skills()`。
- [x] 新增 CLI 子命令：`tiangong skill refresh`。
- [x] 新增 CLI 子命令：`tiangong skill gc`。
- [x] `gc_skills()` 能识别 orphan `skill::*::*` MCP server。
- [x] `gc_skills()` 能识别 `mcp-lock.json` 中无 Skill 引用的孤儿锁条目。
- [x] GC 默认只报告，用户确认后才删除 MCP server 或递减引用计数。

### 11. Skill 激活期 MCP 缺失处理

- [x] 从 `skills/<id>/skill.toml` 的 `[mcp]` / `requires.mcp` 读取托管 MCP 声明。
- [x] 手动安装 Skill 时不自动安装 MCP。
- [x] 激活时发现缺失托管 MCP，返回 `SkillActivationError::MissingMcp`。
- [x] GUI/CLI 收到缺失 MCP 错误后提示用户确认补装。
- [x] 托管 MCP server 命名继续使用 `skill::<id>::<mcp_id>`。

### 12. UI 管理面板懒加载

- [x] Skill 列表页只调用轻量 `list_skills()`。
- [x] 打开详情时调用 `get_skill_detail(id)`。
- [x] 列表页展示非法目录 / orphan MCP 的非阻塞告警。
- [x] 启停开关直接写 `skill.toml.available`。
- [x] 手动拷贝 Skill 后点击刷新即可出现，无需重启。

### 13. `skill doctor` 诊断工具

- [x] 新增 CLI 子命令：`tiangong skill doctor`。
- [x] 诊断缺失 `skill.toml` 的目录。
- [x] 诊断目录名与 `skill.toml.id` 不一致。
- [x] 诊断 `SKILL.md` 缺失或 entry 指向不存在。
- [x] 诊断托管 MCP 引用缺失或孤儿。

---

## P3 - Phase D：清理与文档

### 14. 删除旧注册机制

- [x] 删除 `skills-lock.json` 相关 Skill 注册读写代码（仅保留旧布局迁移备份）。
- [x] 删除 `skills.json.installed[]` 作为注册事实源的代码路径。
- [x] 删除 `installed/<id>/<version>/` 新写入路径。
- [x] 保留 `mcp-lock.json` 相关代码。
- [x] 清理不再使用的类型、字段和迁移兼容分支。

### 15. 文档与示例更新

- [x] 更新用户文档：手动安装 Skill = 拷贝目录到 `~/.tiangong/skills/<id>/`。
- [x] 更新开发文档：`skill.toml.available` 字段语义。
- [x] 更新示例 Skill 为新平铺布局。
- [x] 更新 Tauri command 契约说明。
- [x] 更新 RFC-0003 中被 RFC-0007 修订的章节引用。

### 16. `.legacy` 备份清理

- [x] `tiangong skill gc` 支持列出超过 30 天的 `.legacy` 备份。
- [x] 用户确认后清理过期 `.legacy` 文件。
- [x] 保证 GC 清理失败不影响主程序启动或 Skill 激活。

---

## 当前推荐执行顺序

1. 先同步 `PLAN.md` 和 `docs/requirements.md`，确认 RFC-0007 已进入当前开发主线。
2. 实现 Phase A 的数据结构、manifest `available` 字段、轻量扫描和按需加载。
3. 为 Phase A 补齐单元测试，确保无需改动 UI 即可验证核心行为。
4. 再改造 SkillService 的 install/remove/enable/list/get 主链路。
5. 最后处理迁移器、GC、Tauri/CLI/UI 外围能力。

---

## 验收标准

- [x] `skills/<id>/` 目录存在性唯一决定 Skill 是否安装。
- [x] 手动拷贝/删除 Skill 目录后，下一次扫描或激活可立即感知。
- [x] `skill.toml.available` 完整表达 Skill 启停状态。
- [x] `SKILL.md` 仅在激活、详情查看或检索命中时读入内存。
- [x] 从 RFC-0003 旧布局到 RFC-0007 新布局可自动迁移，失败可回退。
- [x] `mcp-lock.json` 引用计数与所有 Skill 声明的 MCP 依赖汇总一致。
- [x] `tiangong skill gc` 可清理删除 Skill 后遗留的孤儿 MCP 托管条目。
