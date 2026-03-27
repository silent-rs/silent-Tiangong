# 火山方舟视频生成 Skill

使用字节跳动火山方舟（Volcengine Ark）平台的视频生成模型生成视频。

## 前置要求

```bash
# 安装 pipx（如未安装）
brew install pipx   # macOS
# 或 pip install pipx
```

无需手动安装 Python 依赖，`pipx run` 会自动处理。

## 配置

在项目根目录的 `.env` 文件中添加：

```env
ARK_API_KEY=你的火山方舟API密钥
ARK_BASE_URL=https://ark.cn-beijing.volces.com/api/v3
```

## 使用

### 命令行

```bash
pipx run skills/volcengine_video/generate.py --prompt "乌鸦喝水的动画视频"
```

### 在天工中使用

直接说：
- "帮我生成一个乌鸦喝水的视频"
- "做一个日落的延时视频"
