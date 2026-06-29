# RFC 0015：CLI 模块化配置增强

- 状态：In Progress（进行中）
- 日期：2026-06-29
- 作者：Tiangong 工程组
- 关联文档：
  - `docs/requirements.md`
  - `PLAN.md`
  - `TODO.md`
  - `docs/rfc/0002-cli-agent-roadmap.md`（CLI Agent 主线）
  - `docs/server-api.md`

## 1. 背景

天工的桌面应用是主入口，模型、Server、Memory、MCP、Skill 等配置都通过桌面设置页完成。但在纯服务端（Linux 服务器、Docker 容器、无桌面环境）场景下，用户无法启动 GUI，只能依赖 CLI 完成配置。

当前 CLI（`crates/tiangong-entry`）只覆盖了 `cli`（交互式 REPL）、`server`（启停）、`mcp`、`skill`、`update` 五个子命令，**缺乏配置类能力**：

- 无法在命令行配置模型供应商、模型与路由。
- 无法修改 Server 监听地址、端口与鉴权 Token。
- 无法启用/禁用 Memory 或调整 Memory 使用的模型。
- 自定义 Prompt 只能通过桌面设置页编辑，CLI 无法触及。
- 没有一站式诊断命令，纯服务端用户不知道当前环境缺什么。

早期讨论曾考虑引入 `tiangong init` 一站式初始化向导，但该方案会把模型、Server、Memory、MCP、Prompt 混在一个流程里，违背"按需配置"的使用习惯，且对只想改单项配置的用户造成负担。

## 2. 目标

构建 **模块化 CLI 配置体系**，让模型、Server、Memory、自定义 Prompt 等配置可以独立查看、编辑、校验和测试，使纯服务端环境具备与桌面设置页等价的配置能力。

必须达到：

- 提供与桌面设置页等价的"分模块配置能力"，不依赖 GUI。
- CLI 与 Desktop 共享同一份 `~/.tiangong/` 配置，CLI 改动桌面可见，反之亦然。
- 每类配置由独立命令管理，用户按需配置，不强迫走初始化流程。
- 提供 `doctor` 诊断命令，但不强制一次性初始化所有内容。

## 3. 非目标

- **不做 `tiangong init` 一站式初始化向导**：不把模型、Server、Memory、MCP、Prompt 混在一个流程里。
- **不引入第二套 headless 配置格式**：不为服务端单独维护 `tiangong-headless.json`，避免三端行为不一致。
- **不把所有配置塞进 `tiangong config set`**：避免变成"配置路径记忆游戏"，体验差。
- **本 RFC 不新增 GUI 功能**：桌面设置页保持现状，后续如需联动 `custom-prompt.md` 另行评估。
- **本 RFC 不改动 CI 发布流水线**：独立 CLI 二进制（tar.gz）发布产物列为后续候选任务。

## 4. 设计原则

1. **不做 `init`**：CLI 不负责一次性初始化所有东西，而是提供分模块配置能力。
2. **每类配置独立命令管理**：模型归模型、Server 归 Server、Memory 归 Memory、Prompt 归 Prompt，MCP/Skill 沿用并增强现有命令。
3. **CLI 与 Desktop 共享同一份配置**：CLI 修改后 Desktop 能看到，Desktop 修改后 CLI 能读取。
4. **提供诊断，不提供强制初始化**：`doctor` 负责告诉用户还缺什么，但不强制一次性帮用户配置完所有东西。
5. **非交互式为主，交互式为辅**：所有配置命令均支持参数式（脚本/CI 友好）；`model`/`server`/`memory` 额外提供 `configure` 子命令作为交互式向导，引导不熟悉配置结构的用户完成配置。非 TTY 环境（脚本、管道、CI）下 `configure` 会检测到并退出，不会卡住自动化流程。

## 5. 命令结构

最终设计的命令树如下：

```text
tiangong
├── cli
├── server
│   ├── (无子命令，前台启动)
│   ├── --daemon / -d
│   ├── stop
│   ├── status
│   ├── configure          # 交互式向导（监听地址 + Token）
│   ├── config
│   │   ├── show
│   │   └── set
│   └── token
│       ├── show
│       ├── set
│       └── generate
├── model
│   ├── list [providers|models|routes]
│   ├── add-provider
│   ├── remove-provider
│   ├── add-model
│   ├── remove-model
│   ├── configure          # 交互式向导（provider → model → route）
│   ├── route <capability> <model>
│   ├── route list
│   ├── validate
│   └── test [target]
├── memory
│   ├── config show
│   ├── config set
│   ├── configure          # 交互式向导（端点模型选择）
│   ├── enable
│   ├── disable
│   ├── status
│   └── test
├── prompt
│   ├── show
│   ├── set
│   ├── edit
│   ├── clear
│   └── path
├── config
│   ├── path
│   ├── show
│   └── validate
├── doctor
├── mcp
├── skill
└── update
```

## 6. 模块设计

### 6.1 模型配置 `tiangong model`

围绕现有 `ModelsConfig`（`crates/tiangong-core/src/models_config.rs`）的 Provider / Model / Routing 三层结构设计。

**Provider 管理**：

```bash
# 推荐：用环境变量名，api_key 存为 ${VAR} 模板
tiangong model add-provider deepseek \
  --protocol deepseek \
  --base-url https://api.deepseek.com \
  --api-key-env DEEPSEEK_API_KEY

# 或明文写入（不推荐）
tiangong model add-provider deepseek \
  --protocol deepseek \
  --base-url https://api.deepseek.com \
  --api-key sk-xxx
```

- `--api-key-env NAME`：写入配置时存为 `${NAME}` 模板，运行时由 `resolve_api_key` 解析环境变量（复用现有机制）。
- `--api-key VALUE`：明文写入。
- 二者互斥。

**Model 管理**：

```bash
tiangong model add-model deepseek-chat \
  --provider deepseek \
  --model-id deepseek-chat \
  --capability chat
```

支持多 `--capability`（chat / multimodal / image_generation / video_generation / stt / tts / embedding / rerank）。

**Routing 管理**：

```bash
tiangong model route chat deepseek-chat
tiangong model route lite deepseek-lite
tiangong model route image_generation doubao-image
tiangong model route list
```

`<capability>` 对应 `RoutingSlot`（chat / lite / multimodal / image_generation / video_generation / stt / tts / embedding / rerank）。

**模型测试**：

```bash
tiangong model test chat        # 测试 chat 路由
tiangong model test deepseek-chat  # 测试指定模型
```

真实请求模型，验证 API Key 读取、base_url、protocol、model id、返回是否正常。实现复用 `ModelsConfig::to_chat_provider_config()` 与 `SingleProviderClient::list_models(&cfg)`（`crates/tiangong-core/src/model.rs:270`），GET `/models` 作为连通性探针。

**交互式向导**：

```bash
tiangong model configure
```

引导用户依次完成 provider → model → route 三步：选协议（DeepSeek/OpenAI 兼容/Anthropic）、输入供应商名称与 base_url、选择 API Key 方式（环境变量名或明文）、输入模型别名与能力、设置路由槽位。复用 `upsert_provider`/`upsert_model`/`set_route_by_name`（含 capability 校验）。适合首次配置或不熟悉参数结构的用户；脚本环境请用参数式命令。

### 6.2 Server 配置 `tiangong server`

**查看配置**：

```bash
tiangong server config show
```

输出（Token 脱敏）：

```text
host: 127.0.0.1
port: 8080
auth_token: tg_****abcd
```

**修改配置**：

```bash
tiangong server config set --host 0.0.0.0 --port 8080
tiangong server config set --port 9000
```

**Token 管理**：

```bash
tiangong server token generate          # 默认长度
tiangong server token generate --length 48
tiangong server token set tg_xxxxxxxxx
tiangong server token show              # 脱敏显示
```

Token 生成使用 scru128 生成 `tg_` 前缀的随机串（不引入新 crate，避免触发 `deny.toml` 依赖审计）。

**Server 状态**：

```bash
tiangong server status
```

检查：PID 文件是否存在、进程是否存活、端口是否监听、本地 API 是否可访问、当前 Token 是否配置。

**交互式向导**：

```bash
tiangong server configure
```

引导设置监听地址（默认 127.0.0.1）、端口（默认 8080）与 Token（生成随机/手动输入/跳过）。复用 `save_server_config`。

**ServerConfig 统一**：当前 `tiangong-config` 与 `tiangong-server` 各自定义同名 `ServerConfig`，是技术债。本 RFC 统一到 `tiangong-config` 版本，为其补齐 `enabled` 字段与 `load_server_config()` / `save_server_config()`，`tiangong-server` 改为 `pub use` 复用。

### 6.3 Memory 配置 `tiangong memory`

围绕现有 `MemoryConfig`（`crates/tiangong-memory/src/config.rs`）设计。该结构字段为 `model / embedding / rerank`（均为 Option 嵌套端点），无顶层 `enabled` 字段。

**配置查看/设置**：

```bash
tiangong memory config show
tiangong memory config set \
  --llm deepseek-lite \
  --embedding bge-embedding \
  --rerank bge-reranker
```

`--llm <name>` 等参数是**模型名引用**：从 `models.json` 解析该模型及其 provider 端点（base_url / api_key / protocol），填充到 `MemoryConfig` 对应字段。模型不存在则报错并提示先执行 `tiangong model add-model`。这使 model 与 memory 两个模块联动，避免重复填写端点。

**启用/禁用**：

```bash
tiangong memory enable
tiangong memory disable
```

`MemoryConfig` 无 `enabled` 字段，且当前加载逻辑仅按端点是否有效判断是否启用。纯清空端点会导致配置丢失且无法重新 enable，因此本 RFC 采用**轻量标记文件** `~/.tiangong/memory/.disabled`（存在即禁用）实现对称开关，不破坏 `MemoryConfig` 结构、不丢失端点配置。

`memory status` 的"启用状态" = `!标记文件存在 && model 端点有效`。

**测试**：

```bash
tiangong memory test
```

检查配置存在性、模型能力匹配、端点有效性，并照搬 `crates/tiangong-memory/examples/memory_llm_smoke.rs` 模式发送测试请求。

**交互式向导**：

```bash
tiangong memory configure
```

引导选择 Memory 端点模型：先确认启用状态，再从 models.json 已注册模型中选择 Memory LLM（校验 chat 能力）、可选选择 Embedding/Rerank 模型（校验对应能力，可跳过）。复用 `lookup_model_for_llm`/capability 校验，确保不会把错误能力的模型配到对应端点。

### 6.4 自定义 Prompt `tiangong prompt`

自定义 Prompt 作为一级命令（不塞到 `config` 下），因为它是高频且语义独立的配置。

**存储迁移**：当前 `custom_system_prompt` 存储在 `app.json` 的 `AgentConfig` 内（JSON 字符串）。长 Prompt 在 JSON 中难以编辑（换行、转义、diff 不友好）。本 RFC 将事实来源改为独立文件：

```text
~/.tiangong/custom-prompt.md
```

**加载优先级**：

```text
1. custom-prompt.md 存在且非空 → 读取它
2. 否则读取 app.json 中 agent_config.custom_system_prompt（兼容旧配置）
3. 再否则为空
```

**写入行为**：`prompt set` / `prompt edit` 写入 `custom-prompt.md` 时，同时清空 `app.json` 的 `custom_system_prompt` 旧字段，使 `custom-prompt.md` 成为唯一事实来源，消除歧义。

下游注入逻辑（`crates/tiangong-core/src/prompt/sections.rs:172/200`）无需改动，只需在加载 `agent_config` 时用上述优先级回填 `custom_system_prompt` 字段。

**命令**：

```bash
tiangong prompt show                 # 显示内容与字数
tiangong prompt set "..."            # 直接设置
tiangong prompt set --file ./p.md    # 从文件读取
tiangong prompt edit                 # 优先 $EDITOR，回退 vim/nano
tiangong prompt clear                # 清空（删 .md + 清空旧字段）
tiangong prompt path                 # 显示存储路径
```

### 6.5 通用配置 `tiangong config`

只负责通用查看、校验，不改所有东西。

```bash
tiangong config path       # 列出全部配置路径
tiangong config show       # 概览（不展开 JSON）
tiangong config validate   # 仅本地结构校验，不做外部连通性
```

`config validate` 与 `doctor` 区分：

- `config validate`：只校验本地配置结构。
- `doctor`：执行完整环境诊断，可能请求模型、检查端口、检查服务。

`config export` / `config import`（从桌面配置迁移到 Linux Server）不在 0.12.0 清单，列为后续候选。

### 6.6 诊断 `tiangong doctor`

不修改配置，只告诉用户当前环境是否可用。

```bash
tiangong doctor        # 默认不做真实网络请求
tiangong doctor --deep # 执行模型连通性 + 端口探活
```

默认输出：

```text
配置目录            ✅ /home/user/.tiangong
模型配置            ✅ chat -> deepseek-chat
模型连通性          ⏭️ 跳过（使用 --deep 启用）
Server 配置         ✅ 127.0.0.1:8080
Server Token        ✅ 已配置
Memory 配置         ⚠️ 未启用
MCP 配置            ✅ 2 个服务可解析
Skill 目录          ✅ 5 个 Skill 可用
自定义 Prompt       ✅ 已配置，128 字
```

缺配置时给出具体修复命令，例如：

```text
❌ 未配置 chat 模型

可执行：

  tiangong model add-provider deepseek \
    --protocol deepseek \
    --base-url https://api.deepseek.com \
    --api-key-env DEEPSEEK_API_KEY

  tiangong model add-model deepseek-chat \
    --provider deepseek \
    --model-id deepseek-chat \
    --capability chat

  tiangong model route chat deepseek-chat
```

## 7. 最终用户体验

服务端用户不需要 `init`，按需配置：

```bash
# 1. 配模型供应商
tiangong model add-provider deepseek \
  --protocol deepseek \
  --base-url https://api.deepseek.com \
  --api-key-env DEEPSEEK_API_KEY

# 2. 配模型
tiangong model add-model deepseek-chat \
  --provider deepseek \
  --model-id deepseek-chat \
  --capability chat

# 3. 设置 chat 路由
tiangong model route chat deepseek-chat

# 4. 配 Server
tiangong server config set --host 127.0.0.1 --port 8080
tiangong server token generate

# 5. 配自定义 Prompt
tiangong prompt edit

# 6. 检查
tiangong doctor

# 7. 启动
tiangong server -d
```

只想改 Prompt：`tiangong prompt edit`；只想换模型：`tiangong model route chat glm-4.5`；只想关 Memory：`tiangong memory disable`。

## 8. Linux 服务器支持

本 RFC 的 CLI 模块化配置直接服务于此场景：让纯服务端环境具备完整可用的配置能力。

当前 CI（`.github/workflows/release.yml`）只构建 Tauri 安装包（macOS dmg / Linux AppImage / Windows NSIS），根 `[[bin]]`（`src/main.rs`，纯 CLI 二进制）无独立发布产物。Linux 服务器用户需通过源码编译获得二进制：

```bash
cargo build --release
# 产物：target/release/tiangong（无 Tauri/WebKit 依赖）
```

更新机制：`tiangong update --check`（`crates/tiangong-entry/src/update.rs`）仅检查版本，CLI 二进制不能自更新（仅桌面应用支持自动更新）。Linux 服务器需重新编译或下载新版本二进制替换；因配置独立存储在 `~/.tiangong/`，与二进制解耦，更新二进制不丢失配置。

**后续候选**：

- 在 CI 新增独立 build job，产出 `tiangong-<version>-<platform>-<arch>.tar.gz` 纯 CLI 二进制。
- 扩展 `latest.json` 增加 CLI 通道，使 `tiangong update` 支持 CLI 二进制自更新（或提供 `tiangong update --apply` 下载替换）。

详细部署指南见 `docs/linux-server-deployment.md`。

## 9. 里程碑

### M1：文档与 Roadmap（本 RFC + PLAN.md + TODO.md）

锁定 0.12.0 设计方向，作为后续代码实现依据。

### M2：Prompt 独立配置

- `custom-prompt.md` 独立存储 + 加载优先级兼容
- `tiangong prompt show/set/edit/clear/path`

优先实现，因为改动独立、风险低、能快速验证"独立文件 + CLI 命令"模式。

### M3：Server 配置 CLI

- `tiangong server config show/set`
- `tiangong server token show/set/generate`
- `tiangong server status`
- ServerConfig 统一

### M4：模型配置 CLI

- Provider / Model / Routing 增删改查
- `model validate` 与 `model test` 连通性测试

最重要也最复杂，需严谨处理 Provider / Model / Routing 三层。

### M5：Memory 配置 CLI

- `memory config show/set`
- `memory enable/disable`（标记文件方案）
- `memory status/test`

### M6：config + doctor

- `config path/show/validate`
- `doctor` 聚合诊断

前面模块完成后，doctor 才能整合诊断。

## 10. 验收标准

达到以下条件视为 RFC 0015 完成：

- 在无桌面的 Linux 环境中，可通过 CLI 独立完成模型、Server、Memory、自定义 Prompt 等关键配置。
- CLI 修改的配置能被桌面应用读取，桌面修改的配置能被 CLI 读取（同一份 `~/.tiangong/`）。
- `tiangong doctor` 能准确报告环境缺失项并给出修复命令。
- `custom-prompt.md` 成为自定义 Prompt 的事实来源，旧 `custom_system_prompt` 字段保持兼容。
- 各命令具备相应单元测试（`ModelsConfig` 增删、custom-prompt 加载优先级、token 生成、ServerConfig load/save、MemoryConfig 标记文件开关）。
- 通过完整检查链（`cargo fmt --check` + `cargo check --workspace` + `cargo clippy -D warnings` + `cargo nextest run`）。

## 11. 与现有 RFC 的关系

- **RFC 0002（CLI Agent 主线）**：本 RFC 与 0002 正交。0002 关注 CLI 的 Agent 执行能力（规划、工具、验证），本 RFC 关注 CLI 的配置能力。两者共同构成完整的 CLI 使用体验：先配置（本 RFC），再执行任务（0002）。
- 本 RFC 不影响 RFC 0002 的任何里程碑，可并行推进。
