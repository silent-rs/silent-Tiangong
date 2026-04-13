#!/usr/bin/env python3
"""
创建 Skill 的脚本
根据输入参数自动生成新的技能模块
"""

import argparse
import json
import os
from pathlib import Path
from datetime import datetime


def create_skill_yaml(skill_name, display_name, description, triggers, category):
    """生成 skill.yaml 内容"""
    triggers_yaml = "\n".join([f"  - {kw}" for kw in triggers])
    yaml_content = f"""# {display_name}
# 自动生成的技能配置文件

name: {skill_name}
version: 1.0.0
description: {description}

# 触发关键词
trigger_keywords:
{triggers_yaml}

# 参数定义
parameters:
  input:
    type: string
    description: 输入内容
    required: true

# 执行配置
execution:
  type: python
  entry_point: {skill_name}.py
  working_directory: skills/{skill_name}

# 错误处理
error_handling:
  retry_count: 2
  fallback_message: "抱歉，执行失败，请稍后再试。"

# 使用示例
examples:
  - input: "执行{display_name}"
    description: "触发{display_name}功能"
"""
    return yaml_content


def create_python_script(skill_name, display_name):
    """生成 Python 执行脚本"""
    script_content = f'''#!/usr/bin/env python3
"""
{display_name}
自动生成的技能执行脚本
"""

import json
import sys

def main():
    """主函数"""
    # 解析输入参数
    if len(sys.argv) > 1:
        args = json.loads(sys.argv[1])
    else:
        # 从 stdin 读取
        import sys
        args = json.load(sys.stdin)

    # 业务逻辑
    input_data = args.get("input", "")

    # 执行任务
    result = {{
        "status": "success",
        "skill": "{skill_name}",
        "display_name": "{display_name}",
        "input": input_data,
        "output": f"处理完成: {{input_data}}",
        "timestamp": "{datetime.now().isoformat()}"
    }}

    # 输出结果
    print(json.dumps(result, ensure_ascii=False, indent=2))

if __name__ == "__main__":
    main()
'''
    return script_content


def create_readme(skill_name, display_name, description):
    """生成 README.md"""
    readme_content = f"""# {display_name}

## 简介

{description}

## 目录结构

```
{skill_name}/
├── skill.yaml      # 技能配置文件
├── {skill_name}.py  # 执行脚本
└── README.md        # 本文档
```

## 使用方法

### 触发方式

在对话中使用以下关键词即可触发：

- 「{display_name}」
- 具体的触发词...

### 参数说明

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| input | string | 是 | 输入内容 |

### 示例

```json
{{
  "skill": "{skill_name}",
  "input": "测试内容"
}}
```

## 安装说明

1. 将 `{skill_name}` 目录放入项目的 `skills` 目录
2. 确保已安装 Python 3.7+
3. 如有依赖，在目录下创建 `requirements.txt`

## 开发指南

### 修改 skill.yaml

根据需要修改技能配置：

- `trigger_keywords`: 更新触发关键词
- `parameters`: 调整参数定义
- `description`: 更新功能描述

### 修改执行脚本

编辑 `{skill_name}.py` 实现具体业务逻辑。

## 许可证

MIT License
"""
    return readme_content


def main():
    parser = argparse.ArgumentParser(description="创建新的 Skill")
    parser.add_argument("--skill-name", required=True, help="技能名称（英文）")
    parser.add_argument("--display-name", required=True, help="显示名称（中文）")
    parser.add_argument("--description", required=True, help="功能描述")
    parser.add_argument("--triggers", nargs="+", required=True, help="触发关键词")
    parser.add_argument("--category", default="utility", help="技能分类")
    parser.add_argument("--output-dir", default="skills", help="输出目录")

    args = parser.parse_args()

    # 构建输出路径
    skill_dir = Path(args.output_dir) / args.skill_name
    skill_dir.mkdir(parents=True, exist_ok=True)

    # 生成文件
    files = {
        "skill.yaml": create_skill_yaml(
            args.skill_name,
            args.display_name,
            args.description,
            args.triggers,
            args.category,
        ),
        f"{args.skill_name}.py": create_python_script(
            args.skill_name, args.display_name
        ),
        "README.md": create_readme(
            args.skill_name, args.display_name, args.description
        ),
    }

    # 写入文件
    for filename, content in files.items():
        filepath = skill_dir / filename
        filepath.write_text(content, encoding="utf-8")
        print(f"✓ 创建: {filepath}")

    # 生成汇总
    result = {
        "status": "success",
        "skill_name": args.skill_name,
        "display_name": args.display_name,
        "output_directory": str(skill_dir),
        "created_files": list(files.keys()),
        "timestamp": datetime.now().isoformat(),
    }

    print("\n" + "=" * 50)
    print("技能创建完成！")
    print(f"名称: {args.display_name}")
    print(f"目录: {skill_dir}")
    print("=" * 50)

    return json.dumps(result, ensure_ascii=False, indent=2)


if __name__ == "__main__":
    main()
