# Create Skill

## 简介

一个用于自动创建新技能模块的元技能（Meta-Skill）。通过这个技能，AI 助手可以根据用户需求自动生成完整的技能模块结构。

## 目录结构

```
create_skill/
├── skill.yaml      # 技能配置文件
├── create_skill.py  # 执行脚本
└── README.md        # 本文档
```

## 使用方法

### 触发方式

在对话中使用以下关键词即可触发：

- 「创建skill」
- 「新建技能」
- 「添加技能」
- 「创建一个新的skill」
- 「我想添加一个新功能」
- 「create a new skill」

### 参数说明

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| skill_name | string | 是 | 技能名称（英文，snake_case） |
| skill_display_name | string | 是 | 技能显示名称（中文） |
| description | string | 是 | 功能描述 |
| trigger_keywords | array | 是 | 触发关键词列表 |
| category | string | 否 | 技能分类（默认 utility） |

### 分类选项

- `utility`: 工具类
- `search`: 搜索类
- `tool`: 工具类
- `automation`: 自动化类
- `integration`: 集成类

### 命令行使用示例

```bash
python3 create_skill.py \
  --skill-name my_weather \
  --display-name "天气查询" \
  --description "查询指定城市的天气预报" \
  --triggers "查天气" "天气预报" "今天天气怎么样" \
  --category utility
```

### 对话中使用示例

用户：
> "帮我创建一个skill，名称是 my_weather，显示名称是天气查询，功能是查询天气预报"

AI 助手会自动调用这个 create_skill 技能来生成完整的技能模块。

## 生成的文件结构

每个新创建的 skill 包含以下文件：

```
{skill_name}/
├── skill.yaml      # 技能配置文件
├── {skill_name}.py  # 执行脚本
└── README.md        # 本文档
```

## 开发指南

### 修改 skill.yaml

根据需要修改技能配置：

- `trigger_keywords`: 更新触发关键词
- `parameters`: 调整参数定义
- `description`: 更新功能描述

### 修改执行脚本

编辑 `create_skill.py` 实现具体业务逻辑。

## 工作原理

1. 解析用户输入的参数（技能名称、描述、触发词等）
2. 使用模板生成 skill.yaml 配置文件
3. 生成 Python 执行脚本模板
4. 生成 README.md 文档
5. 将所有文件写入目标目录

## 许可证

MIT License
