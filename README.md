# Silent-Tiangong（天工）

> 全功能可扩展的 GUI + CLI + Server 个人智能终端平台

Silent-Tiangong 是一个实现"可对话、可规划、可执行、可扩展、可治理、可远程"的 Agent 能力闭环的个人 AI 中枢。支持多智能体团队协作、通过 Server API 对接各类 IM 通道远程调度，并具备图片/视频等多媒体生成能力。

## 核心能力

- **智能对话** — 意图识别、多轮上下文管理、自动摘要压缩
- **任务规划与执行** — 输入 → 规划 → 多轮执行 → 验证 → 反馈，动态 Step 推进
- **多智能体协作** — 主 Agent 动态组建团队（PM / Developer / Tester 等），Sub Agent 间通过工具调用互相通讯，共享工作区但有文件编辑锁防止冲突
- **工具调用** — 文件读写、命令执行、代码搜索、补丁应用、网页抓取等本地工具
- **MCP 集成** — 标准化 MCP 协议接入外部工具（stdio / HTTP）
- **Skill 管理** — Skill 安装、启停、卸载，按任务意图自动匹配
- **长期记忆** — 基于 SQLite + Tantivy + 向量索引的持久化记忆系统，支持跨会话回忆
- **多媒体生成** — 图片生成（DALL-E）、视频生成（火山方舟）、语音合成/识别（TTS/STT）
- **多通道接入** — 通过 Server API + 外部适配程序对接 Telegram、Discord、飞书/Lark 等 IM 通道
- **权限管理** — 监督模式（高风险操作审批）/ 信任模式（全自动执行），实时切换

## 多智能体协作

天工支持在对话中动态组建多 Agent 团队，每个 Sub Agent 拥有独立的角色、工具集和执行上下文。

### 工作模式

```
用户: "帮我实现用户认证模块"

Main Agent → 创建团队
  ├── @pm    (Project Manager)  — 需求分析、任务拆分、进度跟踪
  ├── @dev   (Developer)        — 代码实现
  └── @test  (Tester)           — 测试用例编写与执行

PM 拆分任务 → @dev 实现 → @dev 提测 → @test 测试 → @test 报告 PM
```

### 核心特性

- **动态团队组建** — 主 Agent 根据任务复杂度自主决定是否创建团队，不额外消耗 API 调用
- **Agent 间通讯** — Sub Agent 通过 `send_message` / `broadcast_message` 工具互相协作
- **文件编辑锁** — 多 Agent 编辑同一文件时自动排队，防止冲突
- **用户交互** — 支持 `@dev`、`@test`、`@all` 等 @提及语法向指定 Agent 发送指令
- **直接推送** — Sub Agent 可直接向用户推送消息（进度、阻塞、提问），无需经主 Agent 转发
- **视角切换** — 前端 Agent Tab 栏可切换查看每个 Agent 的执行细节
- **混合生命周期** — 持久 Agent 跨任务保持上下文，临时 Agent 完成后自动销毁

> 详细设计见 [RFC 0011: 多智能体协作系统](docs/rfc/0011-multi-agent-collaboration.md)

## 架构

Cargo workspace 多 crate 结构：

```
crates/
  tiangong-types/       公共类型定义（消息、会话、任务状态、流事件等）
  tiangong-config/      配置管理（磁盘加载、持久化、日志初始化）
  tiangong-llm          LLM 协议抽象与 Provider 封装（OpenAI 兼容）
  tiangong-anthropic/   Anthropic 协议适配（Messages API + SSE）
  tiangong-core/        核心引擎（Agent 循环、工具调用、MCP、Skill、多 Agent 团队）
  tiangong-memory/      长期记忆系统（Episode 存储、检索、反刍）
  tiangong-cli/         CLI/TUI 前端（ratatui）
  tiangong-entry/       统一入口与命令路由
  tiangong-server/      HTTP REST + WebSocket Server
  tiangong-media/       多媒体生成（图片/视频/语音）
frontend/               桌面 GUI（React + shadcn/ui）
src-tauri/              Tauri 桌面壳
```

### 执行流程

```
用户输入 → ReAct 循环（推理 + 工具调用 + 观察）
              │
              ├── 单 Agent 直接执行
              │
              └── 多 Agent 协作
                    │
                    ├── create_agent → 组建团队
                    ├── send_message → Agent 间通讯
                    ├── lock_file / unlock_file → 文件编辑锁
                    └── dismiss_agent → 解散团队
```

### 交互模型

执行过程按事件模型实时展示：

```
[解释] 我需要读取 Cargo.toml 查看 workspace 成员
  [工具] read_file → OK
[解释] 看到成员列表，现在统计代码行数
  [工具] run_command × 7 → OK
[最终回复] Workspace 包含 11 个成员...
```

多 Agent 模式下，前端按 Agent 分 Tab 展示：

```
[Main] [PM] [Dev] [Test]

当前 Tab: Dev
🔒 locked: src/auth/middleware.rs
📝 write_file: src/auth/middleware.rs
   + JWT 中间件实现...
✅ write_file 完成
📨 send_message → @test "认证模块开发完成，已提测"
```

## 安装

### 安装发布包

在 GitHub Releases 下载当前系统对应的安装包：

- macOS：下载 `.dmg` 安装包，打开后将「天工」拖入「应用程序」目录。
- Windows：下载 `.msi` 或 `.exe` 安装包，按安装向导完成安装。
- Linux：下载 `.AppImage`、`.deb` 或 `.rpm`，按发行版习惯安装或直接运行。

安装后可直接启动桌面应用。

> **macOS 用户注意**：由于当前构建暂未接入 Apple 签名和公证，首次打开时系统可能会提示「"天工"已损坏，无法打开」。这是 macOS Gatekeeper 安全机制导致的，并非应用真的损坏。在终端执行以下命令清除隔离属性即可正常打开：
>
> ```bash
> xattr -cr /Applications/天工.app
> ```

### 命令行入口

桌面安装包内包含同一个 `tiangong` 入口，可用于 CLI、Server 和更新命令。

macOS 可创建软链接：

```bash
ln -s /Applications/天工.app/Contents/MacOS/天工 /usr/local/bin/tiangong
```

Windows 安装后可将安装目录加入 `PATH`。Linux 安装包通常会直接提供可执行入口。

### 在线更新

桌面应用设置页提供版本显示、检查更新和安装更新按钮。也可以通过命令行检查和安装：

```bash
# 只检查是否有新版本
tiangong update --check

# 检查、下载并安装更新
tiangong update
```

在线更新复用 GitHub Release 的更新元数据和签名校验。只有正式发布且上传了 updater 元数据后，才会检测到可用更新。

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

# 检查更新（源码运行时只做检查提示）
cargo run --release -- update --check
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
  logs/                 运行日志（含 Server 后台日志）
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
