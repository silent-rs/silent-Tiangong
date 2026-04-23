# 联网搜索 Skill

使用本目录的 `search.py` 从搜索引擎获取实时网页结果，并以 JSON 结构返回标题、URL、摘要和来源。

## 使用方法

```bash
python3 {skill_dir}/search.py --query "搜索内容" --max-results 5 --engine duckduckgo
```

## 参数

- `--query`：搜索关键词，必填。
- `--max-results`：返回结果数量，默认 5。
- `--engine`：搜索引擎，默认 `duckduckgo`。

## 适用场景

- 查询实时信息。
- 获取网页链接和摘要。
- 为后续调研任务提供初始资料。
