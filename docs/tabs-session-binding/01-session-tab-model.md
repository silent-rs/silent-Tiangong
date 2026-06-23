# 01 - 会话 Tab 数据模型

## 目标

在会话层增加统一工作区 Tab 元数据模型，为浏览器和终端共享 Tab 列表做准备。

## 范围

- `crates/tiangong-core/src/session.rs`
- 仅为编译需要更新测试辅助构造代码

## 依赖

- 前置任务：无。
- 后续任务：02、06、09、12。
- 可并行任务：无。
- 阻塞说明：这是统一 Tab 元数据的基础任务，后续读写命令、浏览器会话恢复、前端类型和持久化都依赖该模型。

## 任务

- 新增 `TabKind`，取值为 `browser` 和 `terminal`。
- 新增 `TabState`，字段包含：
  - `id`
  - `kind`
  - `title`
  - `url`
  - `created_at`
- 在 `Session` 增加：
  - `tabs: Vec<TabState>`
  - `active_tab_id: Option<String>`
- 新增字段必须带 `serde(default)`。
- `active_tab_id` 序列化时允许为空。

## 不做

- 不增加 Tauri command。
- 不创建终端或浏览器运行实例。
- 不改前端。

## 验收

- 老会话 JSON 缺少 `tabs` / `active_tab_id` 时仍可反序列化。
- 新会话序列化后包含默认空 Tab 列表。
- 测试辅助结构都补齐字段，编译不报缺字段。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
