# 火山方舟视频生成

使用字节跳动火山方舟（Volcengine Ark）平台的 Seedance 模型生成视频。

## 使用方法

当用户请求生成视频时，使用 `run_command` 工具执行：

```bash
pipx run {skill_dir}/generate.py --prompt "视频描述（英文效果更好）"
```

## 参数

- `--prompt`（必需）：视频内容描述
- `--model`（可选）：模型名称或接入点 ID
- `--max-wait`（可选）：最大等待秒数，默认 300

## 输出

成功：`{"ok": true, "video_url": "https://..."}`
失败：`{"ok": false, "error": "原因"}`

## 触发词

生成视频、制作视频、做个视频、generate video、create video
