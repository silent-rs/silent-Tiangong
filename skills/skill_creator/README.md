# Skill Creator - 技能创建器

一个用于快速生成天工 AI 技能模板的元技能。

## 功能特性

- 🚀 快速创建新的 skill 目录结构
- 📝 自动生成标准化的配置文件
- 🎨 支持多种技能类型模板
- ⚙️ 可自定义配置参数
- ✅ 自动验证生成的文件

## 安装

### 方法一：作为独立脚本使用

```bash
# 进入脚本目录
cd /Users/hubertshelley/Documents/silent/tiangong/skills/skill_creator

# 直接运行
python3 create_skill.py --name "my_skill" --type http
```

### 方法二：集成到天工系统

1. 确保天工系统能够识别 skills 目录
2. 在天工配置中添加此技能的路径
3. 通过天工的技能系统调用

## 使用方法

### 基本用法

```bash
# 创建一个基础的 HTTP 技能
python3 create_skill.py --name "weather_query" --type http

# 创建一个 Python 脚本技能
python3 create_skill.py --name "data_analysis" --type python

# 创建一个 Shell 脚本技能
python3 create_skill.py --name "system_monitor" --type shell
```

### 完整参数

```bash
python3 create_skill.py \
  --name "skill_name" \           # 技能名称（必需）
  --type "http|python|shell" \    # 技能类型（必需）
  --description "技能描述" \       # 技能描述
  --author "作者名" \              # 作者信息
  --version "1.0.0" \             # 版本号
  --output "/path/to/skills"      # 输出目录（可选）
```

### 示例

#### 示例 1: 创建天气查询技能

```bash
python3 create_skill.py \
  --name "weather_query" \
  --type http \
  --description "查询指定城市的天气信息"
```

生成的文件结构：
```
weather_query/
├── skill.yaml        # 技能配置
├── README.md         # 使用文档
└── examples/         # 示例文件
```

#### 示例 2: 创建数据分析技能

```bash
python3 create_skill.py \
  --name "data_analysis" \
  --type python \
  --description "分析数据并生成报告"
```

生成的文件结构：
```
data_analysis/
├── skill.yaml        # 技能配置
├── analyze.py        # Python 脚本
├── requirements.txt  # 依赖列表
└── README.md         # 使用文档
```

## 支持的技能类型

### 1. HTTP 技能 (http)
适用于调用外部 API 或 Web 服务的技能。

**特点：**
- 自动生成 HTTP 请求模板
- 包含请求/响应处理逻辑
- 支持多种 HTTP 方法

**适用场景：**
- 查询天气、新闻等信息
- 调用第三方 API
- 获取网络资源

### 2. Python 技能 (python)
适用于需要复杂逻辑处理的技能。

**特点：**
- 生成标准 Python 脚本
- 包含参数解析和错误处理
- 自动生成 requirements.txt

**适用场景：**
- 数据分析和处理
- 文件操作
- 复杂计算任务

### 3. Shell 技能 (shell)
适用于系统操作和命令行工具的技能。

**特点：**
- 生成 Shell 脚本
- 包含参数验证
- 支持多种 Shell 解释器

**适用场景：**
- 系统监控
- 文件管理
- 自动化任务

## 配置文件说明

### skill.yaml 结构

```yaml
name: skill_name              # 技能名称
version: 1.0.0                # 版本号
description: 技能描述          # 详细描述
author: 作者名                 # 作者信息

# 触发关键词
trigger_keywords:
  - 关键词1
  - 关键词2

# 参数定义
parameters:
  param_name:
    type: string              # 参数类型
    description: 参数描述
    required: true            # 是否必需

# 执行配置
command:
  type: http|python|shell     # 技能类型
  # ... 其他配置
```

## 高级功能

### 自定义模板

可以创建自定义模板来生成特定类型的技能：

```bash
# 使用自定义模板
python3 create_skill.py \
  --name "custom_skill" \
  --template "/path/to/template.yaml"
```

### 批量创建

```bash
# 批量创建多个技能
for skill in weather news translation; do
  python3 create_skill.py --name "${skill}_query" --type http
done
```

### 验证生成的技能

```bash
# 创建后验证
python3 create_skill.py --name "test_skill" --type http --validate
```

## 最佳实践

1. **命名规范**
   - 使用小写字母和下划线
   - 名称应具有描述性
   - 避免使用特殊字符

2. **版本管理**
   - 使用语义化版本号 (semver)
   - 记录每次更新的变更

3. **文档完善**
   - 提供清晰的使用说明
   - 包含实际使用示例
   - 说明参数和返回值

4. **错误处理**
   - 提供友好的错误信息
   - 实现适当的重试机制
   - 记录必要的日志

## 故障排除

### 常见问题

1. **权限错误**
   ```bash
   chmod +x create_skill.py
   ```

2. **Python 版本不兼容**
   - 需要 Python 3.6 或更高版本
   - 检查: `python3 --version`

3. **输出目录不存在**
   - 脚本会自动创建所需目录
   - 或使用 `--output` 指定已存在的目录

## 更新日志

### v1.0.0 (2024-01-20)
- 初始版本发布
- 支持 HTTP、Python、Shell 三种类型
- 自动生成标准化配置文件
- 包含完整的模板系统

## 贡献指南

欢迎贡献代码和建议！

1. Fork 本项目
2. 创建特性分支 (`git checkout -b feature/AmazingFeature`)
3. 提交更改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 开启 Pull Request

## 许可证

MIT License

## 联系方式

如有问题或建议，请通过以下方式联系：
- 提交 Issue
- 发送邮件至项目维护者

---

**提示**: 这是一个元技能，用于创建其他技能。使用它可以大大提高开发新技能的效率！
