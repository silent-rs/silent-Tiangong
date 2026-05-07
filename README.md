# Silent-Tiangong（天工）

> 全功能可扩展的 GUI + CLI + Server 个人智能终端平台

Silent-Tiangong 是一个实现"可对话、可规划、可执行、可扩展、可治理、可远程"的 Agent 能力闭环的个人 AI 中枢。支持通过 Connector 接入各类 IM 通道远程调度，并具备图片/视频等多媒体生成能力。

## 核心能力

- **智能对话** — 意图识别、多轮上下文管理、自动摘要压缩
- **任务规划与执行** — 输入 → 规划 → 多轮执行 → 验证 → 反馈，动态 Step 推进
- **工具调用** — 文件读写、命令执行、代码搜索、补丁应用、网页抓取等本地工具
- **MCP 集成** — 标准化 MCP 协议接入外部工具（stdio / HTTP）
- **Skill 管理** — Skill 安装、启停、卸载，按任务意图自动匹配
- **长期记忆** — 基于 SQLite + Tantivy + 向量索引的持久化记忆系统，支持跨会话回忆
- **多媒体生成** — 图片生成（DALL-E）、视频生成（火山方舟）、语音合成/识别（TTS/STT）
- **多通道接入** — Telegram、Discord、飞书/Lark、Webhook Connector
- **多 Agent 协作** — Worker 并行执行、任务拆分与协调
- **权限管理** — 监督模式（高风险操作审批）/ 信任模式（全自动执行），实时切换

## 架构

Cargo workspace 多 crate 结构：

```
crates/
  tiangong-types/       公共类型定义（消息、会话、任务状态等）
  tiangong-config/      配置管理（磁盘加载、持久化、日志初始化）
  tiangong-llm          LLM 协议抽象与 Provider 封装（OpenAI 兼容）
  tiangong-anthropic/   Anthropic 协议适配（Messages API + SSE）
  tiangong-core/        核心引擎（Agent 循环、工具调用、MCP、Skill）
  tiangong-memory/      长期记忆系统（Episode 存储、检索、反刍）
  tiangong-cli/         CLI/TUI 前端（ratatui）
  tiangong-entry/       统一入口与命令路由
  tiangong-server/      HTTP REST + WebSocket Server
  tiangong-connector/   IM 通道适配（Telegram/Discord/Lark/Webhook）
  tiangong-media/       多媒体生成（图片/视频/语音）
frontend/               桌面 GUI（React + shadcn/ui）
src-tauri/              Tauri 桌面壳
```

### 执行流程

```
用户输入 → 意图分类 → 规划（planning）→ 多轮执行（ReAct 循环）→ 验证 → 最终回复
                         ↓                    ↓
                    简单对话直答         工具调用 / MCP / Skill
```

### 交互模型

执行过程按事件模型实时展示：

```
[解释] 我需要读取 Cargo.toml 查看 workspace 成员
  [工具] read_file → OK
[解释] 看到成员列表，现在统计代码行数
  [工具] run_shell × 7 → OK
[最终回复] Workspace 包含 11 个成员...
```

## 运行

```bash
# 桌面 GUI 模式（默认）
cargo run --release

# CLI 模式
cargo run --release -- cli

# Server 模式
cargo run --release -- server
cargo run --release -- server -d    # 后台运行
cargo run --release -- server stop  # 停止
```

## 配置

存储目录：`~/.tiangong/`

```
~/.tiangong/
  app.json              应用主配置
  models.json           模型配置（Provider + Model + Routing）
  skills.json           Skill 配置
  mcp.json              MCP 配置
  sessions/             会话持久化
  media/                生成的媒体文件
  memory/               长期记忆数据（SQLite + Tantivy 索引）
```

模型配置采用 Provider + Model + Routing 三层架构，`api_key` 支持 `${ENV_VAR}` 环境变量引用。

## 技术栈

| 领域 | 技术 |
|------|------|
| 语言 | Rust (edition 2024) |
| 桌面 GUI | Tauri + React + shadcn/ui |
| CLI/TUI | ratatui + crossterm |
| Server | silent (HTTP/WS) |
| 异步运行时 | tokio |
| LLM 协议 | async-openai（OpenAI 兼容）、tiangong-anthropic（Anthropic） |
| MCP | rmcp |
| 记忆存储 | rusqlite（SQLCipher）、tantivy（全文检索）、qdrant（向量索引） |
| Telegram | teloxide |
| Discord | serenity |
| 序列化 | serde / serde_json |
| ID 生成 | scru128 |

## 开发

```bash
# 检查
cargo check --workspace

# Lint
cargo clippy --workspace --all-targets -- -D warnings

# 格式化
cargo fmt

# 前端构建
cd frontend && yarn build
```

## 许可证

Apache License 2.0
