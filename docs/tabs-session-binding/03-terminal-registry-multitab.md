# 03 - 终端多 Tab 注册表

## 目标

把终端插件从单会话单 PTY 改为单会话多 PTY，为多个终端 Tab 提供后端运行时。

## 范围

- `crates/plugins/tiangong-plugin-terminal/src/session_pty.rs`
- `crates/plugins/tiangong-plugin-terminal/src/types.rs`
- 必要时更新内部类型引用

## 依赖

- 前置任务：01。
- 后续任务：04、10、12、13。
- 可并行任务：02、06、08。
- 阻塞说明：需要统一 Tab id 作为终端 Tab 的持久化元数据基础。

## 任务

- 新增 `SessionTabs`：
  - `tabs: HashMap<tab_id, SessionPty>`
  - `active_tab_id: Option<String>`
  - 会话级 `TerminalActivityTracker`
- `SessionPty` 增加：
  - `tab_id`
  - `title`
  - `created_at`
- `SessionPtyRegistry.sessions` 改为 `HashMap<session_id, Arc<SessionTabs>>`。
- 新增复合 id 解析：`session_id:tab_id`。
- 日志路径改为 `terminal-<tab_id>.log`。
- 草稿 session 转正时迁移所有 Tab。
- workspace 切换时销毁所有会话下的所有 Tab。

## 不做

- 不实现命令执行调度策略。
- 不新增前端组件。
- 不改浏览器。

## 验收

- 同一 session 可创建多个 `SessionPty`。
- 每个 Tab 拥有独立日志文件。
- 使用复合 id 可定位指定 Tab。
- 未指定 tab id 时可解析到活跃 Tab。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
