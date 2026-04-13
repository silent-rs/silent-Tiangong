# app_state 重构设计稿

## 1. 背景

当前 `src/core/app_state/` 已完成按文件拆分，但核心对象仍然是单一的 `TiangongState`：

- 它同时持有会话态、模型配置态、MCP/Skill 配置态、运行态、持久化路径、运行时对象。
- 它同时负责初始化、配置变更、会话切换、Turn 生命周期、MCP 管理、Skill 管理、持久化与恢复。
- 各子文件本质上仍是同一个大 `impl TiangongState` 的横向拆分，职责边界在类型层面尚未真正解耦。

这会带来几个直接问题：

- 状态与行为强耦合，任何改动都容易波及整个 `TiangongState`。
- 持久化、运行时调度、业务状态变更混在一起，不利于推导不变量。
- UI 层只能依赖一个巨型状态对象，后续很难做精细刷新、事件订阅和单元验证。
- Skill / MCP / Session 的生命周期没有独立 owner，后续扩展事务、审计、回滚会继续堆在 `TiangongState` 上。

## 2. 目标

本次重构的目标不是“继续拆文件”，而是把 `app_state` 设计成真正分层的状态系统。

目标如下：

- `TiangongState` 仅作为 UI 层入口 facade，不再承载全部业务细节。
- 状态按领域拆成独立 slice，每个 slice 只维护本领域数据与不变量。
- 持久化、运行时编排、Skill/MCP 管理从 state 中抽出为 service。
- 写操作统一走 coordinator / service，避免任意方法直接改多个领域状态。
- 后续支持事务、审计、回滚、异步任务恢复时，不需要继续膨胀 `TiangongState`。

非目标：

- 本轮不更换 TUI/UI 对外调用方式。
- 本轮不改现有存储格式（`app.json`、`skills.json`、`mcp.json`、`sessions/*.json`）。
- 本轮不引入全局异步状态管理框架。

## 3. 当前问题归纳

### 3.1 状态和服务混合

`TiangongState` 当前既是：

- 状态容器
- 应用启动器
- 持久化仓库
- 运行时调度器
- Skill 管理器
- MCP 管理器

这意味着一个“切换会话”类的小动作，也可能间接依赖：

- 持久化路径
- runtime 重建
- auto resume
- run snapshot 更新

### 3.2 不变量不集中

当前比较关键的不变量包括：

- `active_session_id` 必须指向存在的 session
- `pending_turn` 与 `run.status` 必须匹配
- `model_config` / `agent_config` 更新后，`runtime` 必须同步重建
- `skills.json` / `mcp.json` / lock 文件要保持一致

这些约束散落在多个方法里，没有一个明确的“所有者”。

### 3.3 持久化边界不清晰

目前 state 内部直接持有：

- `app_storage_path`
- `skills_config_path`
- `mcp_config_path`
- `mcp_capability_cache_path`
- `sessions_dir_path`

这说明“业务状态”和“存储介质定位”耦合在一起。后续如果做：

- 测试态内存仓库
- 导入导出
- 多 profile
- 临时会话工作区

都会很别扭。

## 4. 目标结构

### 4.1 顶层结构

建议把 `TiangongState` 改成 facade，内部组合多个独立组件：

```rust
pub struct TiangongState {
    store: AppStore,
    services: AppServices,
}
```

其中：

- `AppStore` 只负责内存状态
- `AppServices` 只负责副作用与外部系统交互

### 4.2 AppStore

```rust
pub struct AppStore {
    sessions: SessionState,
    provider: ProviderState,
    agent: AgentState,
    runtime: RuntimeState,
    ui: UiState,
}
```

#### SessionState

负责：

- 会话列表
- 当前激活会话
- 会话标题草稿
- 输入草稿

建议结构：

```rust
pub struct SessionState {
    sessions: Vec<Session>,
    active_session_id: String,
    session_title_draft: String,
    input_draft: String,
}
```

#### ProviderState

负责：

- 当前模型配置
- provider 设置草稿
- 模型列表

```rust
pub struct ProviderState {
    model_config: ModelProviderConfig,
    settings_api_auth_token_draft: String,
    settings_api_base_url_draft: String,
    settings_api_timeout_ms_draft: String,
    settings_api_model_draft: String,
    model_list: Vec<String>,
}
```

#### AgentState

负责：

- `AgentConfig`
- Skill/MCP 配置的内存态
- capability cache 的内存视图

```rust
pub struct AgentState {
    config: AgentConfig,
    mcp_tools_cache: BTreeMap<String, Vec<McpToolMeta>>,
}
```

说明：

- `AgentConfig` 仍可继续包含 `skills` 与 `mcp` 两块配置，但 owner 应变成 `AgentState`，不是顶层 `TiangongState`。
- 后续如果要进一步拆开，也可以演进成 `SkillState` + `McpState`。

#### RuntimeState

负责：

- `RunSnapshot`
- 当前 pending turn
- 运行态临时索引

```rust
pub struct RuntimeState {
    run: RunSnapshot,
    pending_turn: Option<PendingTurnState>,
}
```

建议把当前 `PendingTurn` 改名为 `PendingTurnState`，明确它是运行态数据而不是 service。

#### UiState

当前 UI 草稿状态不多，但建议预留：

```rust
pub struct UiState {
    // 预留 TUI 对话框选择、筛选、排序、临时提示等
}
```

这样后续 TUI 交互状态不会继续回流到业务状态中。

## 5. 服务层设计

### 5.1 AppServices

```rust
pub struct AppServices {
    repository: AppRepository,
    runtime_factory: RuntimeFactory,
    turn_service: TurnService,
    skill_service: SkillService,
    mcp_service: McpService,
}
```

### 5.2 Repository

把现有 `storage.rs` / `storage_utils.rs` 收敛成仓库层：

```rust
pub struct AppRepository {
    paths: AppPaths,
}
```

#### AppPaths

```rust
pub struct AppPaths {
    app_json: PathBuf,
    skills_json: PathBuf,
    mcp_json: PathBuf,
    mcp_tools_cache_json: PathBuf,
    sessions_dir: PathBuf,
    skills_root: PathBuf,
}
```

Repository 负责：

- `load_bootstrap_state`
- `save_app_state`
- `save_session`
- `delete_session`
- `save_agent_configs`
- `sync_lock_files`
- `load_capability_cache`

原则：

- state 不再直接接触路径
- 所有磁盘读写只走 repository

### 5.3 RuntimeFactory

当前 `rebuild_runtime_for_agent_config` 的根因是：

- runtime 的创建依赖 `model_config`
- runtime 的创建依赖 `agent_config`

建议把它收敛成：

```rust
pub struct RuntimeFactory;

impl RuntimeFactory {
    pub fn build(
        provider: &ProviderState,
        agent: &AgentState,
    ) -> RuntimeEngine { ... }
}
```

这样所有“改配置后重建 runtime”的逻辑都变成显式依赖，不再散落在多个方法里。

### 5.4 TurnService

当前 `turns.rs` 干了三件事：

- 发起 turn
- 接收 worker 事件
- 写回 session / run snapshot

建议拆成 service：

```rust
pub struct TurnService;
```

职责：

- `start_turn(store, services)`
- `poll_turn(store, services)`
- `cancel_turn(store, services)`
- `finish_turn_success(store, services)`
- `finish_turn_error(store, services)`

原则：

- `SessionState` 只保存数据
- Turn 生命周期由 `TurnService` 编排

### 5.5 SkillService

当前 `skills.rs` 既做路径校验，又做安装、删除、启停、转换、复制、落盘。

建议拆成：

```rust
pub struct SkillService;
```

职责：

- 校验 skill 源
- 分析并准备安装源
- 调用转换 agent
- 管理 installed records
- 触发配置校验
- 触发 repository 持久化

### 5.6 McpService

当前 `mcp_settings.rs` 负责：

- server 注册
- 启停
- 删除
- 配置项更新
- capability scheduler 刷新

建议统一收敛到：

```rust
pub struct McpService;
```

职责：

- MCP server 配置变更
- MCP capability cache 同步
- scheduler 配置与刷新
- 与 system prompt 注入所需元数据对齐

## 6. 对外 API 设计

为了兼容现有 UI，`TiangongState` 对外仍保留高层方法，但内部转发到 slice/service。

例如：

```rust
impl TiangongState {
    pub fn switch_session(&mut self, session_id: &str) {
        SessionActions::switch_active(&mut self.store.sessions, session_id);
        self.services
            .repository
            .save_app_state(&self.store);
        self.services
            .turn_service
            .try_auto_resume(&mut self.store, &self.services);
    }
}
```

后续可进一步演进成：

- `state.sessions().active()`
- `state.provider().current_model()`
- `state.agent().mcp_servers()`

而不是所有读接口都挂在 `TiangongState` 顶层。

## 7. 状态变更原则

建议明确以下规则：

### 7.1 Slice 只维护本地不变量

例如：

- `SessionState` 保证 active session 合法
- `ProviderState` 保证草稿字段与已保存字段的关系清晰
- `RuntimeState` 保证 pending turn 与 run snapshot 一致

### 7.2 跨 slice 变更只允许通过 service / coordinator

例如：

- 保存 provider 设置后重建 runtime
- 安装 skill 后更新 agent config、lock、runtime、持久化
- 切换 session 后尝试 auto resume

这些都不应由某个 slice 直接改其他 slice。

### 7.3 持久化不是状态逻辑的一部分

状态先变，持久化后跟。

如果后续需要事务，可以演进成：

- 先生成变更计划
- repository 提交
- 失败时回滚 store

## 8. 建议的目录结构

建议最终演进到如下结构：

```text
src/core/app_state/
  mod.rs
  facade.rs
  store.rs
  services.rs
  lifecycle.rs
  repository/
    mod.rs
    paths.rs
    app_repo.rs
    session_repo.rs
    lock_repo.rs
  slices/
    mod.rs
    session_state.rs
    provider_state.rs
    agent_state.rs
    runtime_state.rs
    ui_state.rs
  services/
    mod.rs
    turn_service.rs
    skill_service.rs
    mcp_service.rs
    runtime_factory.rs
  turn/
    mod.rs
    pending.rs
    events.rs
    worker.rs
  formatting.rs
  tests/
    mod.rs
    skill_install.rs
    session_flow.rs
    provider_settings.rs
```

## 9. 分阶段迁移方案

### Phase A：抽 Store 壳

目标：

- 新增 `AppStore`
- 将 `TiangongState` 字段迁移到 `store`
- `TiangongState` 暂时仍保留原方法签名

收益：

- 先建立“状态容器”和“外层 facade”边界

### Phase B：抽 Repository

目标：

- 将 `storage.rs` / `storage_utils.rs` 迁到 repository
- `TiangongState` 不再直接持有路径字段

收益：

- 路径与读写副作用从 state 中剥离

### Phase C：抽 RuntimeFactory + TurnService

目标：

- `rebuild_runtime_for_agent_config` 移到 factory
- turn 启动、轮询、结束改由 service 负责

收益：

- 清理最复杂的跨领域写操作

### Phase D：抽 SkillService / McpService

目标：

- skill / mcp 配置变更不再由 `TiangongState` 自己处理

收益：

- 为事务、回滚、审计做准备

### Phase E：瘦身 facade

目标：

- `TiangongState` 仅保留 UI 需要的高层入口
- 内部直接字段访问全部改成 `store.xxx`

最终结果：

- `TiangongState` 从“上帝对象”退化为“协调入口”

## 10. 当前代码到目标结构的映射

### 当前模块 -> 目标归属

- `lifecycle.rs`
  - 迁入 `facade.rs` + `services/runtime_factory.rs` + `repository/app_repo.rs`
- `sessions.rs`
  - 读接口迁入 `slices/session_state.rs`
  - 跨领域操作迁入 `services/turn_service.rs`
- `turns.rs`
  - 迁入 `turn/` + `services/turn_service.rs`
- `storage.rs`
  - 迁入 `repository/`
- `storage_utils.rs`
  - 迁入 `repository/paths.rs` 和仓库内辅助函数
- `skills.rs`
  - 迁入 `services/skill_service.rs`
- `mcp_settings.rs`
  - 迁入 `services/mcp_service.rs`
- `provider_settings.rs`
  - 状态部分迁入 `slices/provider_state.rs`
  - 副作用部分迁入 `services/runtime_factory.rs`

## 11. 风险与注意事项

### 11.1 先抽边界，再抽行为

不要一边改结构，一边重写业务逻辑。否则很难判断问题来自：

- 重构边界
- 行为变更
- 状态不变量破坏

### 11.2 保持序列化模型稳定

当前磁盘格式已经被 CLI/TUI 与实际数据依赖。重构阶段不应顺手改：

- `app.json`
- `skills.json`
- `mcp.json`
- `sessions/*.json`

### 11.3 runtime 重建必须统一出口

后续所有影响运行时配置的变更，都应只走一个统一方法，否则仍会出现：

- 有些配置改了但 runtime 没刷新
- 有些配置刷新了但 scheduler 没同步

## 12. 建议的下一步

如果按这份设计稿继续落地，建议顺序固定为：

1. 先抽 `AppStore`
2. 再抽 `AppRepository`
3. 然后处理 `TurnService`
4. 最后再拆 `SkillService` / `McpService`

这样可以优先消除最大的结构问题：状态与副作用耦合。
