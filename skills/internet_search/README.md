# 联网搜索 Skill

## 功能描述
这个 Skill 提供联网搜索功能，可以从互联网获取实时信息并返回结构化的搜索结果。

## 支持的搜索引擎
- **DuckDuckGo** (默认) - 无需 API key，开箱即用
- **Google** - 需要配置 Google Custom Search API
- **Bing** - 需要配置 Bing Search API

## 使用方法

### 基本用法
```bash
python3 search.py --query "搜索内容" --max-results 5 --engine duckduckgo
```

### 参数说明
- `--query`: (必填) 搜索查询内容
- `--max-results`: (可选) 返回的最大结果数量，默认 5
- `--engine`: (可选) 搜索引擎选择，默认 duckduckgo

## 依赖安装

### DuckDuckGo 搜索
```bash
pip install duckduckgo-search
```

### Google 搜索 (可选)
需要配置 Google Custom Search API:
1. 获取 Google API Key
2. 获取 Custom Search Engine ID
3. 在脚本中配置相应的认证信息

### Bing 搜索 (可选)
需要配置 Bing Search API:
1. 获取 Azure Cognitive Services API Key
2. 在脚本中配置相应的认证信息

## 输出格式
返回 JSON 格式的搜索结果：
```json
{
  "query": "搜索查询",
  "engine": "duckduckgo",
  "total_results": 5,
  "results": [
    {
      "title": "结果标题",
      "url": "https://example.com",
      "snippet": "结果摘要",
      "source": "duckduckgo"
    }
  ]
}
```

## 触发关键词
- 搜索
- 联网搜索
- 网上搜索
- 在线搜索
- 查询
- 搜一下
- 百度一下
- Google

## 示例

### 示例 1: 基本搜索
```json
{
  "query": "天工 AI 最新功能",
  "max_results": 5
}
```

### 示例 2: 指定搜索引擎
```json
{
  "query": "Python 异步编程最佳实践",
  "max_results": 10,
  "search_engine": "google"
}
```

## 注意事项
1. 首次使用前请确保安装了必要的依赖包
2. DuckDuckGo 搜索无需 API key，推荐作为默认选项
3. 如需使用 Google 或 Bing，请先配置相应的 API 认证信息
4. 搜索结果数量可能少于请求的最大数量，取决于搜索引擎返回的实际结果

## 版本历史
- v1.0.0 - 初始版本，支持 DuckDuckGo/Google/Bing 搜索引擎
