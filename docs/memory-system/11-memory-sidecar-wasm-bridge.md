# Memory Sidecar + WASM 桥接架构

> 状态：草案（RFC）
> 关联：#301（WASM 插件平台）、#321（memory WASM 化）
> 日期：2026-07-30

## 背景与动机

memory system 的 WASM 化推进到「纯逻辑下沉 + 读写 host import」后，遇到了存储层的硬限制：

- **SQLite（SQLCipher）**：依赖 OpenSSL C 库，无法编译进 wasm
- **tantivy**：依赖 mmap 内存映射，WASI Preview 2 无 mmap 原语
- **lancedb**：依赖 datafusion，官方不支持 wasm，无替代路径

三者都触到 wasm 沙箱的能力边界（无 mmap、无系统级 C 库、4GB 地址空间、单线程）。这意味着「把存储搬进 wasm」这条路走不通。

但项目已有一套成熟的 sidecar 机制（飞书/微信/QQ 三个 bot 独立制品），以及 memory 自身的 TCP IPC + 文件选举基础设施。把它们组合起来，可以绕开存储下沉的难题，同时获得进程隔离和 WASM 逻辑封装的双重收益。

## 目标

- memory 作为**独立 sidecar 进程**运行，承载全部存储（SQLite/tantivy/lancedb）原生运行
- sidecar 二进制由项目构建，支持**下载地址获取 + 自动启动**
- **WASM 插件做桥接**：承载 memory 的纯逻辑（规划/提取/整理/反刍编排），经 host request 向 sidecar 发起存储请求
- Core（CLI/Server/Desktop）作为 sidecar 的客户端，复用现有 TCP IPC
- 不退化任何现有能力（加密、全文检索、向量检索全部保留）

## 非目标

- 不把 SQLite/tantivy/lancedb 搬进 wasm（已论证不可行）
- 不在本轮实现完整代码，本 RFC 只做架构设计与可行性论证
- 不改变 memory 对外 API（MemoryHandle 语义不变）

## 架构总览

```
┌─────────────────────────────────────────────────────────────┐
│ Core 进程（CLI / Server / Desktop）                          │
│                                                             │
│  ┌──────────────┐   host import     ┌──────────────────┐    │
│  │ WASM 组件     │ ──────────────►   │ host_state       │    │
│  │ (memory 逻辑) │  recall/write     │ (MemoryHandle)   │    │
│  │ 规划/提取/整理 │  ← result         │  Remote 模式      │    │
│  │ 反刍编排       │                   │                  │    │
│  └──────────────┘                   └────────┬─────────┘    │
│                                               │ TCP IPC      │
│  ┌──────────────────┐                         │ (现有协议)    │
│  │ MemorySidecar    │ spawn + 监控             │              │
│  │ Manager          │─────────────────────────┼─►            │
│  │ (下载/启动/健康)  │                         │              │
│  └──────────────────┘                         │              │
└───────────────────────────────────────────────┼──────────────┘
                                                │
                            ┌───────────────────▼───────────────┐
                            │ Memory Sidecar 进程（独立二进制）   │
                            │                                   │
                            │  MemoryActor（Leader）             │
                            │  ├── SQLite（SQLCipher 加密）       │
                            │  ├── tantivy（BM25 全文检索）       │
                            │  ├── lancedb（向量检索）            │
                            │  └── 反刍/提取/注入编排             │
                            │                                   │
                            │  IpcServer（写 endpoint 文件）      │
                            └───────────────────────────────────┘
```

### 三个进程角色的职责

| 角色 | 位置 | 职责 |
|---|---|---|
| **Core** | 主进程 | 业务编排、模型调用、会话管理；持有 MemorySidecarManager（下载/启动/监控 sidecar） |
| **WASM 组件** | Core 进程内（Wasmtime 沙箱） | memory 纯逻辑：检索规划、结果整理、反刍编排、规则提取。经 host import 间接访问 sidecar |
| **Memory Sidecar** | 独立进程 | 存储与 memory 业务编排（SQLite/tantivy/lancedb + Actor）。原生运行，无 wasm 限制 |

## 详细设计

### 1. Sidecar 进程模型

sidecar 是一个独立的可执行文件 `tiangong-memory-sidecar`，职责：

1. 启动时获取 Leader lease（复用现有 `election::try_acquire_leader_lease`）
2. 起本进程内 MemoryActor（`start_memory_with_options`，原生 SQLite/tantivy/lancedb）
3. 起 IpcServer 监听 `127.0.0.1:0` 动态端口，写 endpoint 文件（复用现有 `ipc::spawn_memory_bridge`）
4. 心跳维持 Leader 地位（复用现有 `spawn_heartbeat`）
5. 接受 Core 的 TCP 连接，处理 `MemoryIpcRequestPayload`（协议不变）

**关键复用**：sidecar 进程就是现有 election 里的「Leader 进程」，只是从「调用方进程内」变成「独立子进程」。Leader.json / endpoint 文件 / token 鉴权 / JSON-Lines 协议全部不动。

**新增工作**：一个极简的 `main.rs`（解析参数 → 获取 lease → 起 actor + IPC server → 阻塞等待）。

### 2. MemorySidecarManager（生命周期管理）

参照 `BotRuntime` 设计，在 `tiangong-memory`（或新 crate）中新增 `MemorySidecarManager`：

```rust
pub struct MemorySidecarManager {
    // 下载与校验（复用 tiangong-bots 的 Downloader 模式）
    downloader: SidecarDownloader,
    // 进程记录（防 PID 复用，复用 tiangong-bots 的 ProcessRecord 模式）
    process_record: ProcessRecord,
    // 存储布局
    storage_root: PathBuf,
}

impl MemorySidecarManager {
    /// 下载/更新 sidecar 二进制（参照 BotRuntime::install 的事务性替换）
    pub async fn install(&self, version: Option<&str>) -> Result<()>;

    /// 启动 sidecar 进程（spawn + 监控，参照 supervisor::spawn_supervised）
    pub async fn start(&self) -> Result<()>;

    /// 停止 sidecar 进程
    pub async fn stop(&self) -> Result<()>;

    /// 健康检查（进程存活 + endpoint 文件存在）
    pub fn health(&self) -> SidecarHealth;

    /// 确保 sidecar 运行（未运行则启动），返回 MemoryHandle（Remote 模式）
    pub async fn ensure_running(&self) -> Result<MemoryHandle>;
}
```

**存储布局**（参照 bots，放在 storage_root/memory-sidecar/）：
```
~/.tiangong/memory-sidecar/
  tiangong-memory-sidecar       # 二进制
  tiangong-memory-sidecar.pid   # 进程记录（防 PID 复用）
  sidecar.log                   # 日志
  version.json                  # 版本记录
```

### 3. 下载地址获取

参照 bots 的三级目录发现（catalog → 索引 → 制品）：

```
OSS 目录：
  memory-sidecar/catalog.json                    # 总目录
  memory-sidecar/<tag>/tiangong-memory-sidecar-<target>   # 制品
  memory-sidecar/<tag>/<target>.sha256           # 校验
```

`SidecarManifest`（参照 `BotManifest`）：
```rust
pub struct SidecarManifest {
    pub version: String,
    pub min_app_version: String,
    pub platforms: BTreeMap<String, SidecarArtifact>,  // key = "darwin-aarch64" 等
}

pub struct SidecarArtifact {
    pub url: String,
    pub checksum: String,  // "sha256:<hex>"
}
```

下载流程：拉 catalog → 取当前平台制品 → SHA256 校验 → 事务性文件替换（复用 `paths::reject_symlink` 防护）。

### 4. 启动方式

参照 bots 的两条路径：

**Desktop（supervised，崩溃重启）**：
- `spawn_supervised`：tokio::process::Command + supervisor 线程
- 崩溃指数退避自动重启（2s..60s）
- stdout/stderr 重定向到日志
- kill_on_drop 保证不泄漏

**CLI（detached，脱离父进程）**：
- spawn_detached + setsid
- 靠 PID 记录识别，不监督

启动后通过 endpoint 文件（`~/.tiangong/memory/runtime/<service>.json`）发现 sidecar 的 TCP 端口，建立 Remote 模式的 MemoryHandle。

### 5. WASM 桥接层（精简：只做桥接）

WASM 组件**只做桥接**——不承载 memory 纯逻辑，不做内部编排。编排全部由 sidecar 完成（它有完整的 recall_context / rumination，还是带 LLM 的完整版）。

#### 精简原则：一个通用 request 能力

host 只暴露**一个通用 request 方法**，把全部 memory 操作代理给 sidecar，替代逐个硬编码的细碎 host 方法：

```wit
interface memory-store {
    /// 通用请求：转发到 sidecar，复用其全部 20 个操作（零新增 WIT）。
    /// method 对应 MemoryIpcRequestPayload 的变体名（如 "recall"、"write_episode"）。
    /// payload 为对应入参的 JSON 文本；返回结果序列化为 JSON 文本。
    request: func(
        method: string,
        payload: string,
    ) -> result<string, memory-store-error>;
}
```

WASM 用哪个操作，传对应 method + payload JSON 即可：

```
WASM 组件（只做桥接）
  │ request("recall_context", MemoryRecallRequest JSON)
  │ request("run_enhanced_micro_rumination", EnhancedTurnResult JSON)
  │ request("write_episode", Episode JSON)
  │ ...
  │
  ▼ host import（memory-store.request，唯一方法）
  │
host_state（Core 进程）
  │ 构造 MemoryIpcRequestPayload → block_on(MemoryHandle 发送)
  │ MemoryHandle = Remote 模式
  ▼
TCP IPC（现有协议，endpoint 文件发现）
  │
Memory Sidecar 进程
  └── Actor 完成完整编排（含 LLM/检索/存储）→ 返回结果
```

#### 砍掉的冗余

| 砍掉的 | 原因 |
|---|---|
| memory-store.recall / write-episode / update-injection / submit-candidate / upsert-manual-memory | 全部由通用 request 替代，不再逐个硬编码 |
| WASM 纯逻辑导出（rerank-fuse / plan-recall-fallback / synthesize-fallback） | sidecar 内部已有相同逻辑（且是带 LLM 的完整版） |
| WASM 内的 rerank/anchor/synthesize/text_utils 模块 | 同上，编排不归 WASM |
| injection-level 枚举、recall-hit/recall-response 等 record | 不再需要 WIT 表达 memory 数据结构，全走 JSON |

#### 精简后的 WIT 全貌

```wit
interface clock { ... }            // 标准（或自定义时钟）

interface memory-store {
    variant memory-store-error { message(string), disabled, }
    /// 通用请求，转发到 sidecar 的全部操作。
    request: func(method: string, payload: string)
        -> result<string, memory-store-error>;
}

interface plugin {
    // 插件导出：describe / tool-specs / handle-tool / prompt-sections / shutdown / on-config-updated
    // handle-tool 内部调 memory-store.request 完成召回，结果原样返回
}
```

host_state 只需实现一个 `request` 方法（构造 payload → block_on → 返回 JSON），不再为每个 memory 操作写专门的反序列化 + block_on。

### 6. 配置注入

sidecar 的配置（模型端点、embedding 配置等）经环境变量注入，参照 bots 的 env 注入模式：

- sidecar 启动时由 Manager 注入 `TIANGONG_MEMORY_*` 环境变量
- sidecar 读取后构造 `MemoryOptions`（复用现有 `MemoryConfig::load_or_default` + 环境变量覆盖）
- 存储路径由 `MEMORY_BASE_DIR` 指定（默认 `~/.tiangong/memory`）

### 7. CI 构建

参照 bots 的 release workflow，新增 `release-memory-sidecar.yml`：

- 触发：`workflow_dispatch` 或 tag `memory-sidecar-v*`
- 构建矩阵：4 平台（darwin-aarch64 / darwin-x86_64 / linux-x86_64 / windows-x86_64）
- 产物：单独的可执行文件 `tiangong-memory-sidecar-<target>`
- 上传 OSS：`memory-sidecar/<tag>/`
- 生成索引：`memory-sidecar/memory-sidecar-index.json`

sidecar 是独立的编译 crate（`bots/memory-sidecar/` 或 `crates/tiangong-memory-sidecar/`），有自己的 `[[bin]]` + 空 `[workspace]` 表（避免被主 workspace 收编），有独立 `Cargo.lock`。

## 复用度分析

| 设施 | 来源 | 复用方式 |
|---|---|---|
| 下载 + SHA256 校验 + 事务替换 | `tiangong-bots/downloader.rs` | 抽象成通用 SidecarDownloader 或直接引用模式 |
| 进程监督 + 崩溃重启 | `tiangong-bots/supervisor.rs` | 复用 spawn_supervised 模式 |
| PID 记录 + 防复用 | `tiangong-bots/process_record.rs` | 复用 ProcessRecord + verify_identity |
| 存储布局 + symlink 防护 | `tiangong-bots/paths.rs` | 复用 reject_symlink + 目录模式 |
| TCP IPC + endpoint + token | `tiangong-memory/ipc/` | **直接复用**，协议不变 |
| Leader 选举 + 心跳 | `tiangong-memory/election/` | **直接复用**，sidecar 当 Leader |
| MemoryActor + 存储 | `tiangong-memory/actor.rs` + store | **直接复用**，sidecar 内起 actor |
| memory-store host import | `tiangong-plugin-runtime/host_state.rs` | **零改动**，MemoryHandle 已抽象 Local/Remote |
| WASM 纯逻辑 | `tiangong-plugin-memory-wasm/` | **已有**，规划/提取/整理/反刍编排 |

**结论**：核心机制 80% 可复用。新增工作主要是 `MemorySidecarManager`（参照 BotRuntime 写一个 memory 版本）和 sidecar 的 `main.rs`（极简入口）。

## 与现有架构的兼容过渡

过渡分三步，每步独立可用：

### 第一步：sidecar 二进制 + Manager（不碰 Core）
- 构建 sidecar 二进制（复用 election/ipc/actor）
- 实现 MemorySidecarManager（下载/启动/监控）
- 验证 sidecar 能独立运行、接受 IPC 请求

### 第二步：Core 改用 sidecar（保留降级）
- registry 改为优先连 sidecar（Remote 模式）
- sidecar 不可用时降级为进程内 actor（现有 Leader 模式）
- WASM 桥接路径不变（host import 经 MemoryHandle，Local/Remote 透明）

### 第三步：WASM 承载完整逻辑（最终形态）
- 反刍编排下沉到 WASM
- sidecar 只负责存储（接收 host import 的原子操作）
- 进程内 actor 可选保留作降级兜底

## 风险与权衡

| 风险 | 影响 | 缓解 |
|---|---|---|
| sidecar 进程崩溃 | memory 短暂不可用 | supervisor 指数退避重启 + 降级进程内 actor |
| 多实例冲突（多开天工） | 多个 Core 连同一 sidecar | 现有 Leader 选举已解决（唯一 Leader） |
| 下载/启动失败 | memory 不可用 | 降级进程内 actor + 明确错误提示 |
| TCP IPC 性能 | 序列化开销 | 现有协议已是 JSON-Lines，个人记忆库量级足够 |
| 二进制体积 | 多一个可执行文件 | sidecar 复用 tiangong-memory crate，增量有限 |

## 决策摘要

1. memory sidecar 作为独立进程运行，承载全部存储（原生，无 wasm 限制）
2. 复用现有 TCP IPC + 文件选举 + token 鉴权（协议不变）
3. WASM 组件承载纯逻辑，经 host import 间接访问 sidecar（host 管理连接）
4. 参照 bots 机制实现下载/启动/监控（MemorySidecarManager）
5. 过渡可降级到进程内 actor，保证不中断现有功能
6. 核心机制 80% 可复用，主要新增工作是 Manager + sidecar 入口
