# T2 - BrowserManager 持有 registry

## 目标
让 `BrowserManager` 从持单个全局 `Arc<Mutex<BrowserState>>` 改为持 `Arc<BrowserSessionRegistry>`，现有方法经 `active_state()` 获取当前 session 的 state，行为对前端命令不变。

## 范围
- `crates/plugins/tiangong-plugin-browser/src/manager.rs`（`BrowserManager` 结构 + 所有方法的 state 获取）
- `crates/plugins/tiangong-plugin-browser/src/lib.rs`（`BrowserPluginState` + `init()`）

## 依赖
- 前置任务：T1（registry 数据结构）
- 后续任务：T3（fetcher 路由）、T5（switch_session 重写）
- 可并行任务：无
- 阻塞说明：manager 必须先持 registry，T3 的 command 路由和 T5 的 switch_session 才有落脚点。

## 任务
- `BrowserManager` 结构改为 `{ registry: Arc<BrowserSessionRegistry> }`，移除直接持有的 `state`。
- 新增内部 helper：`fn state(&self) -> Arc<Mutex<BrowserState>>`（= `self.registry.active_state()`，所有现有方法用它替代原 `self.state`）。
- 新增 `fn session_state(&self, id: &str) -> Arc<Mutex<BrowserState>>`（显式路由，供 agent 路径用）。
- `global_history` / `zoom_factor` 上移到进程级（registry 或独立单例），各 session state 不再各持一份——决策：放到 `BrowserSessionRegistry` 进程级字段。
- `BrowserPluginState`：`manager: BrowserManager` 保留语义，`init()` 创建 registry 并传入 manager。
- 确保所有现有方法经 `self.state()` 间接获取，编译通过、行为不变（单 session 场景下 active_state 回退首个注册）。

## 不做
- 不改方法签名（外部调用方不变）。
- 不改 switch_session 的销毁逻辑（T5）。
- 不改轮询/data_directory（T5）。
- 不改 BrowserCommand（T3）。

## 验收
- `BrowserManager` 经 registry 获取 state，现有单 session 场景行为不变。
- `global_history`/`zoom` 进程级共享，不被 per-session 副本割裂。
- `cargo check -p tiangong-plugin-browser` 通过。

## 验证
- `cargo check -p tiangong-plugin-browser`
- 手动（可选）：单 session 打开/导航/tab 操作正常。
