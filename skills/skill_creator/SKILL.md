# Skill 创建器

创建、导入和适配天工 Skill 的交互式工作流。支持从零创建新 Skill，也支持从 GitHub 等外部来源导入并适配为天工格式。

## 触发词

创建skill、新建技能、导入skill、创建一个skill、skill_creator

## 工作流

### 一、需求分析

1. 确认用户想创建什么功能的 Skill
2. 如果用户提供了外部 URL（如 GitHub 仓库），用 `run_command` 获取信息：
   ```bash
   curl -sL https://api.github.com/repos/{owner}/{repo}/contents/{path}
   ```
3. 分析外部 Skill 的功能、依赖和调用方式

### 二、创建 Skill 文件

工作目录：`~/.tiangong/skills/workspace/{skill-id}/`

#### 必需文件

**1. skill.toml** - 元数据声明
```toml
id = "my-skill"                    # 唯一 ID（kebab-case）
name = "技能显示名称"               # 中文名称
version = "0.1.0"                   # 版本号（跟随天工版本）
entry = "SKILL.md"                  # 入口文件

[source]
type = "local"

[requires]
env = ["API_KEY"]                   # 所需环境变量（可选）

[permissions]
# fs_read = true                   # 文件读取权限
# fs_write = true                  # 文件写入权限
# cmd_exec = true                  # 命令执行权限
# net = true                       # 网络访问权限
```

**2. SKILL.md** - 技能使用说明（给 Agent 看的指令文档）
```markdown
# 技能名称

简要功能描述。

## 调用方法

使用 `run_command` 执行：
\`\`\`bash
python3 {skill_dir}/main.py --arg1 value1
\`\`\`

## 全部参数

| 参数 | 必需 | 说明 |
|------|------|------|
| `--arg1` | 是 | 参数说明 |

## 输出

JSON 格式：`{"ok": true, "result": "..."}`

## 触发词

关键词1、关键词2
```

**3. 执行脚本** - Python/Shell 脚本
- Python 脚本使用 `argparse` 解析参数
- 输出 JSON 格式结果到 stdout
- 错误信息输出到 stderr

### 三、安装 Skill

使用内置工具 `install_skill` 安装：
- `path`: Skill 目录路径（如 `~/.tiangong/skills/workspace/my-skill`）
- `enabled`: true

### 四、注册 MCP 服务器（如需）

如果 Skill 依赖 MCP 服务器，使用内置工具 `register_mcp_server`：
- `name`: 服务器名称
- `command`: 启动命令完整路径（如 `/usr/local/bin/uvx`）
- `args`: 命令参数（如 `["mcp-server-name"]`）
- `env`: 环境变量（如 `{"API_KEY": "xxx"}`）
- `transport`: 传输方式（`stdio` 或 `http`）

### 五、验证

安装完成后告知用户：
- Skill 名称和 ID
- 触发词
- 环境变量配置提示（如有）

## 管理工具参考

以下内置工具可直接调用：

| 工具 | 参数 | 说明 |
|------|------|------|
| `register_mcp_server` | name, command, args?, env?, transport?, endpoint? | 注册 MCP 服务器 |
| `remove_mcp_server` | name | 移除 MCP 服务器 |
| `set_mcp_enabled` | name, enabled | 启停 MCP 服务器 |
| `install_skill` | path, enabled? | 安装本地 Skill |
| `remove_skill` | id | 卸载 Skill |
| `set_skill_enabled` | id, enabled | 启停 Skill |

## 外部 Skill 适配指引

将外部 Skill（如 OpenAI Skills、Claude Skills）转换为天工格式：

1. **分析入口文件**：找到 README 或配置文件，理解功能和依赖
2. **提取执行逻辑**：识别核心脚本和参数
3. **创建 skill.toml**：填写 id、name、requires
4. **编写 SKILL.md**：将调用方式转换为 `run_command` + 脚本参数格式
5. **适配脚本**：确保脚本可通过命令行参数调用，输出 JSON
6. **处理依赖**：Python 脚本头部用 `/// script` 声明 PEP 723 依赖，或在 SKILL.md 中说明使用 `pipx run` / `uvx`
