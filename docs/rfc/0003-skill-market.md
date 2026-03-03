# RFC-0003: Tiangong Skill 能力与管理（对齐 MCP）

- 状态：Draft
- 作者：Tiangong Core Team
- 创建时间：2026-03-03
- 更新时间：2026-03-03
- 版本：0.2.0

---

## 1. 背景

当前 Tiangong 已具备：

- Skills 本地扫描与意图匹配
- MCP server 注册/启停/删除/筛选
- Agent 配置持久化与校验

但 Skill 仍停留在“目录扫描提示”，缺少安装、启停、卸载、依赖管理与权限收敛能力。

本 RFC 定义一个可落地的 Skill 管理方案，核心要求是：

- Skill 的管理方式与 MCP 一致
- Skill 依赖的 MCP 以统一配置模型接入
- 先交付本地可用 MVP，再扩展远程 Registry

---

## 2. 范围与非目标

### 2.1 MVP 范围（本 RFC 首期）

- 本地 Skill 安装/启停/卸载/列表/详情
- Skill 依赖 MCP 的安装与绑定
- Skill 与 MCP 的统一审计记录
- CLI/TUI 管理入口（与 `/mcp` 交互风格一致）
- 安全策略最小闭环（路径、命令、网络、权限提示）

### 2.2 非目标（首期不做）

- 商业化支付、评分系统、推荐系统
- GUI Web Market
- 去中心化分发/P2P
- 复杂组织权限体系

---

## 3. 设计原则

1. 管理一致性：Skill 与 MCP 都支持“查看、筛选、启停、增删、校验、持久化”。
2. 配置一致性：Skill 和 MCP 均落在 `~/.tiangong/app.json` 管理域内。
3. 依赖可追踪：Skill -> MCP 的映射关系可审计、可回滚。
4. 默认安全：最小权限、默认拒绝高风险能力。
5. 渐进交付：先本地源，后远程 Registry。

---

## 4. 总体架构

```text
Tiangong CLI/TUI
    ├─ SkillManager
    │   ├─ SkillInstaller
    │   ├─ SkillIndex
    │   └─ SkillPermissionEngine
    └─ McpManager (existing)
        ├─ McpInstaller
        └─ McpProcessManager
```

说明：

- `SkillManager` 负责 Skill 生命周期管理。
- `McpManager` 继续负责 MCP 配置与连接生命周期。
- `SkillInstaller` 仅生成受控 MCP 配置，不允许 Skill 注入任意 command/args。

---

## 5. 与 MCP 一致的管理方式

### 5.1 CLI 命令

```bash
tiangong skill                 # 打开 Skill 管理弹窗（类 /mcp）
tiangong skill <query>         # 按关键词筛选
```

后续兼容非交互命令：

```bash
tiangong skill list
tiangong skill install <source>
tiangong skill remove <id>
tiangong skill enable <id>
tiangong skill disable <id>
tiangong skill validate
```

### 5.2 弹窗交互（与 /mcp 一致）

- 列表区：支持筛选、上下选择、显示 enabled/disabled。
- 详情区：显示 Skill 元信息、依赖 MCP、权限摘要、安装来源。
- 操作区：支持启停、删除、添加（本地目录/Git 源）。
- 状态栏：与 MCP 管理反馈一致，输出结构化结果。

---

## 6. Skill 包与元数据

### 6.1 MVP 目录规范

```text
skill-name/
  SKILL.md
  skill.toml      # 推荐，MVP 可选；无则降级读取 SKILL.md
  README.md       # 可选
  assets/         # 可选
```

### 6.2 skill.toml（MVP）

```toml
id = "fs-helper"
name = "FS Helper"
version = "0.1.0"
entry = "SKILL.md"

[source]
type = "local"            # local | git | registry
value = "/abs/path/skill"

[requires]
mcp = [
  { id = "filesystem", source = "npm", package = "@modelcontextprotocol/server-filesystem", version = "0.6.0" }
]

[permissions]
fs_read = ["./**"]
fs_write = ["./src/**"]
cmd_exec = ["git", "cargo"]
net = []
```

### 6.3 兼容策略

- 若缺失 `skill.toml`：从 `SKILL.md` 推断 `name/description`，`id` 使用目录名。
- 若 `skill.toml` 存在：以其为准并校验必填项。

---

## 7. Skill 依赖 MCP 的映射规则

`requires.mcp` 不直接下发为自由命令，必须经过安装器转换为受控 `McpServerConfig`。

### 7.1 命名规则

- 自动生成 MCP server 名：`skill::<skill_id>::<mcp_id>`。
- 禁止与用户手动 MCP server 重名。

### 7.2 配置生成

- `transport` 由依赖来源推导（npm 默认 stdio；http 源默认 http）。
- `command/args` 由安装器模板生成，Skill 包内不可覆盖。
- `env/cwd` 默认空，除非平台内建模板显式允许。

### 7.3 卸载规则

- 卸载 Skill 时仅移除其“托管 MCP server”。
- 被多个 Skill 共享的依赖引用计数归零后才删除实际安装产物。

---

## 8. 存储与锁文件

统一放在用户目录 `~/.tiangong`：

```text
~/.tiangong/
  app.json
  skills/
    registry.toml
    skills-lock.json
    mcp-lock.json
    installed/<skill_id>/<version>/...
```

### 8.1 skills-lock.json（示例）

```json
{
  "fs-helper": {
    "version": "0.1.0",
    "enabled": true,
    "source": "local:/abs/path/skill",
    "installed_at": "2026-03-03 15:30:00",
    "managed_mcp_servers": ["skill::fs-helper::filesystem"]
  }
}
```

### 8.2 mcp-lock.json（示例）

```json
{
  "@modelcontextprotocol/server-filesystem@0.6.0": {
    "path": "/Users/xxx/.tiangong/skills/deps/node/...",
    "ref_count": 1,
    "installed_at": "2026-03-03 15:30:00"
  }
}
```

---

## 9. 安全模型

### 9.1 文件权限

- 所有路径必须经过 canonicalize。
- 必须位于当前 workspace 或用户配置允许目录内。

### 9.2 命令权限

- `cmd_exec` 仅允许白名单命令名，不允许 shell 复合语法。
- 默认拒绝 `bash -c`、链式下载执行等高风险模式。

### 9.3 网络权限

- 默认 deny。
- 首期不开放任意网络，仅允许 MCP HTTP endpoint 显式配置。

### 9.4 安装确认

安装前输出：

- Skill 基本信息
- 权限摘要
- 依赖 MCP 列表
- 将创建/变更的 MCP server 名称

用户确认后继续安装。

---

## 10. 事务与回滚

安装事务：

1. 解析与校验 Skill 包
2. 安装依赖（可重入）
3. 写入 lock
4. 注入 `app.json` 中 Skill/MCP 配置
5. 校验并重建运行时

失败回滚：

- 回滚本次新增的托管 MCP 配置
- 回滚 lock 增量
- 保留错误审计事件

---

## 11. 审计事件

Skill 与 MCP 统一记录结构：

- `event_id`
- `event_type`（skill_install/skill_remove/skill_enable/mcp_install/...）
- `skill_id` / `mcp_server`
- `status`
- `duration_ms`
- `error`
- `timestamp`

---

## 12. 分阶段实施

### Phase A（当前）

- RFC/需求/计划/任务基线对齐
- Skill 数据结构与配置校验扩展

### Phase B（MVP）

- `/skill` 管理弹窗
- 本地目录安装、启停、卸载
- Skill->MCP 自动映射与托管
- lock 文件与回滚

### Phase C（增强）

- Git 源安装
- 非交互 `tiangong skill ...` 子命令
- 远程 registry（只读索引 + 下载）

---

## 13. 验收标准

满足以下条件视为 RFC-0003 MVP 完成：

1. 用户可在 CLI 中完成 Skill 的安装、启停、卸载。
2. Skill 依赖 MCP 可自动安装并以托管方式注册。
3. `app.json`、`skills-lock.json`、`mcp-lock.json` 三者一致。
4. 安装失败可回滚，且能在会话中看到失败原因。
5. Skill/MCP 管理操作均有结构化审计记录。

---

## 14. 与 RFC-0002 的关系

- RFC-0002 的 CLI Agent 主能力继续有效。
- RFC-0003 在其基础上扩展“可管理的 Skill 能力层”。
- 实施顺序：优先本地可用与安全闭环，再扩展市场能力。
