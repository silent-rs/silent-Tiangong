# Skill 文件系统注册表

本文档描述 RFC-0007 生效后的 Skill 管理规则。

## 安装与标识

- Skill 安装事实源是 `~/.tiangong/skills/<id>/` 目录。
- `<id>` 是稳定机器标识，用于目录名、`@skill` 引用、托管 MCP 名称和审计 key。
- `skill.toml.name` 只用于 UI 展示，允许本地化或调整，不参与寻址。
- 系统始终只使用 `skills/<id>/` 当前内容，不维护多版本目录。
- `skill.toml.version` 仅用于展示、审计和诊断。

## 目录结构

```text
~/.tiangong/skills/
  mcp-lock.json
  web-search/
    skill.toml
    SKILL.md
    .env.local
    ...
```

## 启停状态

`skill.toml.available` 是唯一启停状态来源：

- `available = true`：可被 `@skill`、检索匹配和运行时工具激活。
- `available = false`：已安装但不可激活，仍会在管理视图中显示。
- 字段缺失时按 `true` 处理。

## 实时加载

- Skill 列表只读取轻量 manifest 信息，不读取 `SKILL.md` 全文。
- 打开详情、运行时调用 `get_skill_detail` 或检索命中时才读取 `SKILL.md`。
- 手动拷贝目录到 `skills/<id>/` 后，执行刷新即可出现在列表中。
- 手动删除目录后，执行刷新即可从列表中移除。

## MCP 依赖

- `mcp-lock.json` 保留，用于 MCP 依赖引用计数。
- `skills-lock.json` 不再作为注册状态读取或写入，仅作为旧布局迁移输入被备份。
- Skill 声明 `requires.mcp` 后，托管 MCP server 名称固定为 `skill::<id>::<mcp_id>`。
- 激活 Skill 时如缺少托管 MCP，会返回 `SkillActivationError::MissingMcp`，需要用户确认后补充注册对应 MCP。

## 命令

```bash
tiangong skill refresh
tiangong skill gc
tiangong skill gc --apply
tiangong skill doctor
```

- `refresh`：强制重扫 `skills/<id>/`。
- `gc`：默认只报告孤儿托管 MCP server 和孤儿 `mcp-lock` 条目。
- `gc` 也会列出超过 30 天的 `.legacy` 备份。
- `gc --apply`：删除报告中的孤儿托管 MCP server，重写 `mcp-lock.json`，并清理过期 `.legacy` 备份。
- `doctor`：诊断缺失 manifest、id 不一致、entry 缺失、托管 MCP 孤儿等问题。
