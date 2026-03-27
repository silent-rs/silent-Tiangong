# 火山方舟视频生成

使用火山方舟 Seedance 模型生成视频。

## 调用方法

使用 `run_command` 执行：

```bash
pipx run {skill_dir}/generate.py --prompt "视频描述" --output /path/to/output.mp4
```

## 全部参数

| 参数 | 必需 | 说明 |
|------|------|------|
| `--prompt` | 是 | 视频内容描述（英文效果更好） |
| `--output` 或 `-o` | 否 | 下载视频到本地路径 |
| `--model` | 否 | 模型名称，默认 doubao-seedance |
| `--max-wait` | 否 | 最大等待秒数，默认 300 |

**注意：只有以上 4 个参数，没有其他参数。**

## 输出

JSON 格式：`{"ok": true, "video_url": "...", "local_path": "..."}`

## 触发词

生成视频、制作视频、做个视频
