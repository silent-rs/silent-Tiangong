# T1 - per-session 状态注册表骨架

## 目标
建立 `BrowserSessionRegistry`，把浏览器状态从全局单例结构改为 per-session 的 `HashMap` 骨架，并让 `BrowserState` 适配为「单 session 内部」语义。

## 范围
- `crates/plugins/tiangong-plugin-browser/src/session_registry.rs`（新增）
- `crates/plugins/tiangong-plugin-browser/src/manager.rs`（`BrowserState` 字段调整）
- `crates/plugins/tiangong-plugin-browser/src/lib.rs`（模块声明）

## 依赖
- 前置任务：无
- 后续任务：T2（manager 持 registry）、T3（command 路由依赖 registry）
- 可并行任务：无
- 阻塞说明：本任务是 registry 数据结构的建立，T2 及之后才能消费它。

## 任务
- 新建 `session_registry.rs`：`BrowserSessionRegistry` 结构（`sessions: Mutex<HashMap<String, Arc<Mutex<BrowserState>>>>` + `active_session_id: Mutex<Option<String>>`）。
- 实现 `session_state(id)` 懒创建、`active_state()` 取当前（无 active 时回退首个已注册）、`set_active(id)`、`destroy_session(id)`、`active_session_id()`。
- `BrowserState` 新增 `new_empty()` 构造（加载 global_history/zoom 等进程级数据的逻辑保留，但 global_history/zoom 的归属在 T2 处理）。
- `lib.rs` 声明 `pub mod session_registry;`。
- 本任务不改 `BrowserManager` 的持有方式（T2 做），只提供 registry 数据结构 + BrowserState 适配。

## 不做
- 不改 `BrowserManager` 方法签名（T2）。
- 不改 `BrowserCommand`（T3）。
- 不改 handler/commands/watcher。

## 验收
- `session_registry.rs` 存在且 `BrowserSessionRegistry` 可编译。
- `BrowserState::new_empty()` 可构造。
- `cargo check -p tiangong-plugin-browser` 通过。

## 验证
- `cargo check -p tiangong-plugin-browser`
